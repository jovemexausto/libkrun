// SPDX-License-Identifier: Apache-2.0
//
// Event handler for virtio-tpm.
// Follows exactly the same pattern as virtio-rng's event_handler.rs.

use std::os::unix::io::AsRawFd;

use polly::event_manager::{EventManager, Subscriber};
use utils::epoll::{EpollEvent, EventSet};

use super::device::{REQ_INDEX, Tpm};
use crate::virtio::device::VirtioDevice;

impl Tpm {
    pub(crate) fn handle_req_event(&mut self, event: &EpollEvent) {
        debug!("tpm: request queue event");

        let event_set = event.event_set();
        if event_set != EventSet::IN {
            warn!("tpm: unexpected event on request queue: {event_set:?}");
            return;
        }

        if let Err(e) = self.queue_event(REQ_INDEX).read() {
            error!("tpm: failed to read request queue eventfd: {e:?}");
            return;
        }

        if self.process_req() {
            self.device_state.signal_used_queue();
        }
    }

    fn handle_activate_event(&self, event_manager: &mut EventManager) {
        debug!("tpm: activate event");

        if let Err(e) = self.activate_evt.read() {
            error!("tpm: failed to read activate eventfd: {e:?}");
        }

        let self_subscriber = event_manager
            .subscriber(self.activate_evt.as_raw_fd())
            .unwrap();

        event_manager
            .register(
                self.queue_event(REQ_INDEX).as_raw_fd(),
                EpollEvent::new(
                    EventSet::IN,
                    self.queue_event(REQ_INDEX).as_raw_fd() as u64,
                ),
                self_subscriber.clone(),
            )
            .unwrap_or_else(|e| {
                error!("tpm: failed to register queue event with event manager: {e:?}");
            });

        event_manager
            .unregister(self.activate_evt.as_raw_fd())
            .unwrap_or_else(|e| {
                error!("tpm: failed to unregister activate event: {e:?}");
            });
    }
}

impl Subscriber for Tpm {
    fn process(&mut self, event: &EpollEvent, event_manager: &mut EventManager) {
        let source = event.fd();
        let activate_fd = self.activate_evt.as_raw_fd();

        if source == activate_fd {
            self.handle_activate_event(event_manager);
        } else if self.is_activated() {
            let req_fd = self.queue_event(REQ_INDEX).as_raw_fd();
            if source == req_fd {
                self.handle_req_event(event);
            } else {
                warn!("tpm: unexpected event source fd={source}");
            }
        } else {
            warn!("tpm: spurious event before activation, fd={source}");
        }
    }

    fn interest_list(&self) -> Vec<EpollEvent> {
        vec![EpollEvent::new(
            EventSet::IN,
            self.activate_evt.as_raw_fd() as u64,
        )]
    }
}
