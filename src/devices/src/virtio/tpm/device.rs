// SPDX-License-Identifier: Apache-2.0
//
// Virtio TPM device implementation.
//
// Queue protocol (from Linux tpm_virtio driver):
//   The guest submits ONE descriptor chain per TPM command:
//     desc[0]: read-only  — raw TPM command bytes
//     desc[1]: write-only — buffer for TPM response
//
//   The device:
//     1. Reads the command from desc[0]
//     2. Calls backend.execute_command()
//     3. Writes the response into desc[1]
//     4. Returns desc[1].written_len via add_used()

use utils::eventfd::EventFd;
use vm_memory::{Bytes, GuestMemoryMmap};

use super::super::{
    ActivateError, ActivateResult, DeviceQueue, DeviceState, QueueConfig, VirtioDevice,
};
use super::{backend::TpmBackend, defs, defs::uapi, Result, TpmError};
use crate::virtio::InterruptTransport;

/// Index of the single request/response queue.
pub(crate) const REQ_INDEX: usize = 0;

/// Feature bits: only VIRTIO_F_VERSION_1 is required for virtio-tpm.
pub(crate) const AVAIL_FEATURES: u64 = 1u64 << uapi::VIRTIO_F_VERSION_1;

pub struct Tpm {
    queues: Option<Vec<DeviceQueue>>,
    avail_features: u64,
    acked_features: u64,
    pub(crate) activate_evt: EventFd,
    pub(crate) device_state: DeviceState,
    /// The TPM backend (swtpm or other).
    backend: Box<dyn TpmBackend>,
}

impl Tpm {
    pub fn new(backend: Box<dyn TpmBackend>) -> Result<Self> {
        Ok(Self {
            queues: None,
            avail_features: AVAIL_FEATURES,
            acked_features: 0,
            activate_evt: EventFd::new(utils::eventfd::EFD_NONBLOCK).map_err(TpmError::EventFd)?,
            device_state: DeviceState::Inactive,
            backend,
        })
    }

    pub fn id(&self) -> &str {
        defs::TPM_DEV_ID
    }

    pub(crate) fn queue_event(&self, idx: usize) -> &std::sync::Arc<utils::eventfd::EventFd> {
        &self.queues.as_ref().expect("queues should exist")[idx].event
    }

    /// Process all pending descriptor chains in the request queue.
    /// Returns true if at least one descriptor was consumed (caller should
    /// signal the used ring).
    pub fn process_req(&mut self) -> bool {
        let (mem, _interrupt) = match &self.device_state {
            DeviceState::Activated(mem, interrupt) => (mem.clone(), interrupt.clone()),
            DeviceState::Inactive => unreachable!(),
        };

        let queues = self
            .queues
            .as_mut()
            .expect("queues should exist when activated");

        let mut have_used = false;

        while let Some(head) = queues[REQ_INDEX].queue.pop(&mem) {
            let index = head.index;

            // Collect descriptors: expect exactly 2 (cmd=read-only, resp=write-only).
            let descs: Vec<_> = head.into_iter().collect();

            if descs.len() < 2 {
                error!(
                    "tpm: descriptor chain too short ({} desc), expected 2",
                    descs.len()
                );
                // Return the descriptor with 0 bytes written to unblock the guest.
                if let Err(e) = queues[REQ_INDEX].queue.add_used(&mem, index, 0) {
                    error!("tpm: add_used error: {e:?}");
                }
                have_used = true;
                continue;
            }

            let cmd_desc = &descs[0];
            let resp_desc = &descs[1];

            // Validate descriptor directions.
            if !cmd_desc.is_read_only() {
                error!("tpm: first descriptor must be read-only (command)");
                let _ = queues[REQ_INDEX].queue.add_used(&mem, index, 0);
                have_used = true;
                continue;
            }
            if !resp_desc.is_write_only() {
                error!("tpm: second descriptor must be write-only (response)");
                let _ = queues[REQ_INDEX].queue.add_used(&mem, index, 0);
                have_used = true;
                continue;
            }

            let cmd_len = cmd_desc.len as usize;
            if cmd_len > defs::TPM_BUFSIZE {
                error!("tpm: command too large ({cmd_len} > {})", defs::TPM_BUFSIZE);
                let _ = queues[REQ_INDEX].queue.add_used(&mem, index, 0);
                have_used = true;
                continue;
            }

            // Read the TPM command from guest memory.
            let mut cmd_buf = vec![0u8; cmd_len];
            if let Err(e) = mem.read_slice(&mut cmd_buf, cmd_desc.addr) {
                error!("tpm: failed to read command from guest memory: {e:?}");
                let _ = queues[REQ_INDEX].queue.add_used(&mem, index, 0);
                have_used = true;
                continue;
            }

            // Execute command via backend.
            let response = match self.backend.execute_command(&cmd_buf) {
                Ok(r) => r,
                Err(e) => {
                    error!("tpm: backend error: {e}");
                    // Return a TPM_RC_FAILURE response so the guest doesn't hang.
                    // TPM2 error response: tag=TPM_ST_NO_SESSIONS(0x8001), size=10,
                    // code=TPM_RC_FAILURE(0x101)
                    vec![
                        0x80, 0x01, // tag: TPM_ST_NO_SESSIONS
                        0x00, 0x00, 0x00, 0x0a, // size: 10
                        0x00, 0x00, 0x01, 0x01, // rc: TPM_RC_FAILURE
                    ]
                }
            };

            let resp_len = response.len();
            if resp_len > resp_desc.len as usize {
                error!(
                    "tpm: response ({resp_len} bytes) exceeds guest buffer ({} bytes)",
                    resp_desc.len
                );
                let _ = queues[REQ_INDEX].queue.add_used(&mem, index, 0);
                have_used = true;
                continue;
            }

            // Write response into guest memory.
            if let Err(e) = mem.write_slice(&response, resp_desc.addr) {
                error!("tpm: failed to write response to guest memory: {e:?}");
                let _ = queues[REQ_INDEX].queue.add_used(&mem, index, 0);
                have_used = true;
                continue;
            }

            if let Err(e) = queues[REQ_INDEX]
                .queue
                .add_used(&mem, index, resp_len as u32)
            {
                error!("tpm: add_used error: {e:?}");
            }
            have_used = true;
        }

        have_used
    }
}

impl VirtioDevice for Tpm {
    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, acked_features: u64) {
        self.acked_features = acked_features;
    }

    fn device_type(&self) -> u32 {
        uapi::VIRTIO_ID_TPM
    }

    fn device_name(&self) -> &str {
        "tpm"
    }

    fn queue_config(&self) -> &[QueueConfig] {
        &defs::QUEUE_CONFIG
    }

    fn read_config(&self, _offset: u64, _data: &mut [u8]) {
        // virtio-tpm has no device configuration space.
        error!("tpm: guest attempted to read device config (no config space)");
    }

    fn write_config(&mut self, offset: u64, data: &[u8]) {
        warn!(
            "tpm: guest attempted to write device config (offset={offset:#x}, len={})",
            data.len()
        );
    }

    fn activate(
        &mut self,
        mem: GuestMemoryMmap,
        interrupt: InterruptTransport,
        queues: Vec<DeviceQueue>,
    ) -> ActivateResult {
        if queues.len() != defs::NUM_QUEUES {
            error!(
                "tpm: cannot activate: expected {} queue(s), got {}",
                defs::NUM_QUEUES,
                queues.len()
            );
            return Err(ActivateError::BadActivate);
        }

        if self.activate_evt.write(1).is_err() {
            error!("tpm: cannot write to activate_evt");
            return Err(ActivateError::BadActivate);
        }

        self.queues = Some(queues);
        self.device_state = DeviceState::Activated(mem, interrupt);

        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.device_state.is_activated()
    }

    fn reset(&mut self) -> bool {
        self.queues = None;
        self.device_state = DeviceState::Inactive;
        true
    }
}
