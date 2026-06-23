// SPDX-License-Identifier: Apache-2.0
//
// TpmBackend trait and SwtpmBackend implementation.
//
// SwtpmBackend connects to a running swtpm process via two Unix sockets:
//
//   Control channel  (--ctrl type=unixio,path=<ctrl_path>)
//     Lifecycle commands. Wire format: { u32 cmd (big-endian) | optional body }
//
//   Data channel     (established via CMD_SET_DATAFD with SCM_RIGHTS)
//     Actual TPM commands. Wire format: raw TPM bytes (size in TPM header).
//
// Start swtpm before the VM:
//
//   mkdir -p /tmp/swtpm
//   swtpm socket --tpm2 \
//     --tpmstate dir=/tmp/swtpm \
//     --ctrl type=unixio,path=/tmp/swtpm/ctrl.sock \
//     --daemon

use std::io::{IoSlice, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;

use nix::sys::socket::{ControlMessage, MsgFlags, sendmsg};

use super::{Result, TpmError, defs::TPM_BUFSIZE};

// ─── swtpm control channel command codes (big-endian u32 on wire) ────────────

const CMD_GET_CAPABILITY: u32 = 1;
const CMD_INIT: u32 = 2;
const CMD_SHUTDOWN: u32 = 3;
const CMD_SET_DATAFD: u32 = 16; // 0x10

/// PTM_CAP_SET_DATAFD — capability bit we require.
const PTM_CAP_SET_DATAFD: u32 = 1 << 12;

// ─── TpmBackend trait ─────────────────────────────────────────────────────────

/// Abstraction over TPM command execution.
///
/// The device calls `execute_command` with raw TPM bytes and expects raw
/// response bytes back. TPM-level errors (e.g. TPM_RC_FAILURE) are valid
/// responses and must be returned as `Ok(bytes)`, not as `Err`.
pub trait TpmBackend: Send {
    fn execute_command(&mut self, command: &[u8]) -> std::result::Result<Vec<u8>, String>;
}

// ─── SwtpmBackend ─────────────────────────────────────────────────────────────

pub struct SwtpmBackend {
    ctrl: UnixStream,
    data: UnixStream,
}

impl SwtpmBackend {
    /// Connect to swtpm at `ctrl_path` and complete the handshake:
    ///   GET_CAPABILITY → INIT → SET_DATAFD (passes data socket via SCM_RIGHTS)
    pub fn new(ctrl_path: &str) -> Result<Self> {
        let mut ctrl = UnixStream::connect(ctrl_path).map_err(TpmError::BackendConnect)?;

        // 1. Verify swtpm supports SET_DATAFD
        let caps = ctrl_get_capability(&mut ctrl)
            .map_err(TpmError::BackendInit)?;
        if caps & PTM_CAP_SET_DATAFD == 0 {
            return Err(TpmError::BackendInit(
                "swtpm does not support SET_DATAFD; \
                 start with: --ctrl type=unixio,path=<sock>"
                    .into(),
            ));
        }

        // 2. Initialize the TPM
        ctrl_send_init(&mut ctrl).map_err(TpmError::BackendInit)?;

        // 3. Create a socketpair; send one end to swtpm via SCM_RIGHTS
        let (data_local, data_remote) = UnixStream::pair()
            .map_err(|e| TpmError::BackendInit(format!("socketpair: {e}")))?;

        ctrl_set_datafd(&mut ctrl, &data_remote).map_err(TpmError::BackendInit)?;

        // swtpm now owns data_remote; we use data_local.
        drop(data_remote);

        Ok(Self { ctrl, data: data_local })
    }

    /// Graceful shutdown — best effort.
    pub fn shutdown(&mut self) {
        let _ = ctrl_send_raw(&mut self.ctrl, CMD_SHUTDOWN, &[]);
    }
}

impl TpmBackend for SwtpmBackend {
    fn execute_command(&mut self, command: &[u8]) -> std::result::Result<Vec<u8>, String> {
        data_execute(&mut self.data, command)
    }
}

impl Drop for SwtpmBackend {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// ─── Control channel helpers ──────────────────────────────────────────────────

/// Write a control command and read back the response (up to 64 bytes).
fn ctrl_send_raw(
    ctrl: &mut UnixStream,
    cmd: u32,
    body: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    let mut msg = cmd.to_be_bytes().to_vec();
    msg.extend_from_slice(body);
    ctrl.write_all(&msg).map_err(|e| format!("ctrl write cmd={cmd}: {e}"))?;

    let mut resp = vec![0u8; 64];
    let n = ctrl.read(&mut resp).map_err(|e| format!("ctrl read cmd={cmd}: {e}"))?;
    resp.truncate(n);
    Ok(resp)
}

fn ctrl_get_capability(ctrl: &mut UnixStream) -> std::result::Result<u32, String> {
    let resp = ctrl_send_raw(ctrl, CMD_GET_CAPABILITY, &[])?;
    if resp.len() < 8 {
        return Err(format!("GET_CAPABILITY: short response ({} bytes)", resp.len()));
    }
    // Response: { u32 tpm_result | u32 caps } big-endian
    let result = u32::from_be_bytes(resp[0..4].try_into().unwrap());
    if result != 0 {
        return Err(format!("GET_CAPABILITY: tpm_result=0x{result:08x}"));
    }
    Ok(u32::from_be_bytes(resp[4..8].try_into().unwrap()))
}

fn ctrl_send_init(ctrl: &mut UnixStream) -> std::result::Result<(), String> {
    // CMD_INIT body: { u32 init_flags } — 0 = no special flags
    let resp = ctrl_send_raw(ctrl, CMD_INIT, &0u32.to_be_bytes())?;
    if resp.len() < 4 {
        return Err(format!("INIT: short response ({} bytes)", resp.len()));
    }
    let result = u32::from_be_bytes(resp[0..4].try_into().unwrap());
    if result != 0 {
        return Err(format!("INIT: tpm_result=0x{result:08x}"));
    }
    Ok(())
}

/// Pass the data socket fd to swtpm via CMD_SET_DATAFD + SCM_RIGHTS.
///
/// Uses `nix::sys::socket::sendmsg` with `IoSlice` (nix ≥ 0.27 API).
fn ctrl_set_datafd(
    ctrl: &mut UnixStream,
    data_sock: &UnixStream,
) -> std::result::Result<(), String> {
    use std::os::unix::io::AsRawFd;

    let cmd_bytes = CMD_SET_DATAFD.to_be_bytes();
    let iov = [IoSlice::new(&cmd_bytes)];
    let fd = data_sock.as_raw_fd();
    let fds = [fd];
    let cmsg = [ControlMessage::ScmRights(&fds)];

    sendmsg::<()>(
        ctrl.as_fd().as_raw_fd(),
        &iov,
        &cmsg,
        MsgFlags::empty(),
        None,
    )
    .map_err(|e| format!("SET_DATAFD sendmsg: {e}"))?;

    // Read response: 4 bytes ptm_result
    let mut resp = [0u8; 4];
    ctrl.read_exact(&mut resp)
        .map_err(|e| format!("SET_DATAFD read resp: {e}"))?;
    let result = u32::from_be_bytes(resp);
    if result != 0 {
        return Err(format!("SET_DATAFD: tpm_result=0x{result:08x}"));
    }
    Ok(())
}

// ─── Data channel ─────────────────────────────────────────────────────────────

/// Send a raw TPM command and read the raw response.
///
/// TPM packet structure (both command and response):
///   bytes 0..2  — tag     (u16 big-endian)
///   bytes 2..6  — size    (u32 big-endian, total packet length)
///   bytes 6..10 — ordinal/code (u32 big-endian)
///   bytes 10..  — parameters
fn data_execute(
    data: &mut UnixStream,
    command: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    if command.len() < 10 {
        return Err(format!("TPM command too short: {} bytes (min 10)", command.len()));
    }

    data.write_all(command)
        .map_err(|e| format!("data write: {e}"))?;

    // Read the first 10 bytes of the response header.
    let mut header = [0u8; 10];
    data.read_exact(&mut header)
        .map_err(|e| format!("data read header: {e}"))?;

    // Total response length is at bytes 2..6.
    let total_size = u32::from_be_bytes(header[2..6].try_into().unwrap()) as usize;

    if total_size < 10 {
        return Err(format!("TPM response size={total_size} < 10 (invalid)"));
    }
    if total_size > TPM_BUFSIZE {
        return Err(format!(
            "TPM response size={total_size} > TPM_BUFSIZE={TPM_BUFSIZE}"
        ));
    }

    let mut response = vec![0u8; total_size];
    response[..10].copy_from_slice(&header);
    if total_size > 10 {
        data.read_exact(&mut response[10..])
            .map_err(|e| format!("data read body: {e}"))?;
    }

    Ok(response)
}
