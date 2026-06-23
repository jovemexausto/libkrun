// SPDX-License-Identifier: Apache-2.0
//
// virtio-tpm: paravirtualized TPM device for libkrun.
//
// Architecture:
//   Guest (Android/Linux)
//     └─ /dev/tpm0  (tpm_virtio.ko, CONFIG_VIRTIO_TPM=y)
//         └─ virtqueue: [cmd_buf | resp_buf] descriptor chain
//             └─ VirtioTpm (this device, runs on VMM thread)
//                 └─ SwtpmBackend (Unix socket to swtpm process)
//
// The guest driver sends one descriptor chain per TPM command:
//   [read-only:  TPM command bytes ]
//   [write-only: TPM response buffer]
//
// VIRTIO_ID_TPM = 29 (linux/virtio_ids.h)

mod backend;
mod device;
mod event_handler;

pub use self::backend::{SwtpmBackend, TpmBackend};
pub use self::defs::uapi::VIRTIO_ID_TPM as TYPE_TPM;
pub use self::device::Tpm;

mod defs {
    use crate::virtio::QueueConfig;

    pub const TPM_DEV_ID: &str = "virtio_tpm";

    /// Single virtqueue: guest pushes (cmd, resp) descriptor pairs.
    pub const NUM_QUEUES: usize = 1;

    /// Queue size 2: one slot for the command+response descriptor chain.
    /// Matches crosvm's QUEUE_SIZE = 2 for virtio-tpm.
    const QUEUE_SIZE: u16 = 2;

    pub static QUEUE_CONFIG: [QueueConfig; NUM_QUEUES] =
        [QueueConfig::new(QUEUE_SIZE); NUM_QUEUES];

    /// Maximum TPM command or response size (bytes).
    /// Matches Linux tpm.h TPM_BUFSIZE = 4096.
    pub const TPM_BUFSIZE: usize = 4096;

    pub mod uapi {
        /// virtio device ID for TPM (linux/virtio_ids.h: VIRTIO_ID_TPM = 29).
        pub const VIRTIO_ID_TPM: u32 = 29;
        pub const VIRTIO_F_VERSION_1: u32 = 32;
    }
}

#[derive(Debug)]
pub enum TpmError {
    /// Failed to create activate eventfd.
    EventFd(std::io::Error),
    /// Failed to connect to swtpm control socket.
    BackendConnect(std::io::Error),
    /// swtpm handshake failed.
    BackendInit(String),
}

impl std::fmt::Display for TpmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EventFd(e) => write!(f, "tpm: eventfd error: {e}"),
            Self::BackendConnect(e) => write!(f, "tpm: backend connect error: {e}"),
            Self::BackendInit(s) => write!(f, "tpm: backend init error: {s}"),
        }
    }
}

type Result<T> = std::result::Result<T, TpmError>;
