//! The GUI's side of the GUI pipe (`TLP-Desk.exe` of the logged-in user talking to the
//! service). Platform-independent over any `Read + Write`; on Windows,
//! `service::windows::{gui_subscribe, gui_command}` open the real pipe and check that its
//! server is a service (session 0).

use std::io::{Read, Write};

use super::ipc::{
    self, GuiChannel, GuiCommand, GuiEvent, GuiHello, GuiReply, GuiWelcome, ServiceStatus,
    IPC_VERSION,
};
use crate::HostError;

/// A live event stream from the service.
#[derive(Debug)]
pub struct GuiSubscription<C> {
    conn: C,
}

fn hello<C: Read + Write>(conn: &mut C, channel: GuiChannel) -> Result<(), HostError> {
    ipc::write_msg(
        conn,
        &GuiHello {
            version: IPC_VERSION,
            channel,
        },
    )?;
    match ipc::read_msg::<_, GuiWelcome>(conn)? {
        GuiWelcome::Ok { version } if version == IPC_VERSION => Ok(()),
        GuiWelcome::Ok { version } => Err(HostError::IpcVersion {
            expected: IPC_VERSION,
            got: version,
        }),
        GuiWelcome::Refused { error, .. } => Err(HostError::IpcRefused(format!("{error:?}"))),
    }
}

/// Subscribes on `conn`: returns the service's status snapshot and the event stream.
pub fn gui_subscribe_on<C: Read + Write>(
    mut conn: C,
) -> Result<(ServiceStatus, GuiSubscription<C>), HostError> {
    hello(&mut conn, GuiChannel::Events)?;
    match ipc::read_msg::<_, GuiEvent>(&mut conn)? {
        GuiEvent::Status(s) => Ok((s, GuiSubscription { conn })),
        _ => Err(HostError::IpcRefused("service sent no status first".into())),
    }
}

impl<C: Read> GuiSubscription<C> {
    /// Blocks for the next event. [`HostError::IpcClosed`] when the service went away.
    pub fn next_event(&mut self) -> Result<GuiEvent, HostError> {
        ipc::read_msg(&mut self.conn)
    }
}

/// Sends one command on `conn` and returns the service's reply.
pub fn gui_command_on<C: Read + Write>(
    mut conn: C,
    cmd: &GuiCommand,
) -> Result<GuiReply, HostError> {
    hello(&mut conn, GuiChannel::Command)?;
    ipc::write_msg(&mut conn, cmd)?;
    ipc::read_msg(&mut conn)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use tlp_desk_proto::Permissions;

    use super::super::testutil::duplex;
    use super::super::{serve_gui_connection, GuiCommands, GuiHub};
    use super::*;

    #[derive(Default)]
    struct Fake(Mutex<Vec<u64>>);

    impl GuiCommands for Fake {
        fn respond(&self, request: u64, _accept: bool) -> bool {
            self.0.lock().unwrap().push(request);
            true
        }
        fn rotate_code(&self) {}
        fn disconnect(&self, _session: u64) -> bool {
            false
        }
        fn set_permissions(&self, _session: u64, _p: Permissions) -> bool {
            false
        }
    }

    #[test]
    fn client_and_service_talk() {
        let hub = Arc::new(GuiHub::new(7, "حاسوب".into(), "654321".into()));
        let fake = Arc::new(Fake::default());

        let (a, mut b) = duplex();
        let (h, f) = (hub.clone(), fake.clone());
        std::thread::spawn(move || serve_gui_connection(&mut b, &h, &*f, true));
        let (status, mut sub) = gui_subscribe_on(a).unwrap();
        assert_eq!(status.device_id, 7);
        assert_eq!(status.device_name, "حاسوب");
        hub.publish(GuiEvent::CodeChanged {
            code: "000111".into(),
        });
        assert_eq!(
            sub.next_event().unwrap(),
            GuiEvent::CodeChanged {
                code: "000111".into()
            }
        );

        let (a, mut b) = duplex();
        let (h, f) = (hub.clone(), fake.clone());
        std::thread::spawn(move || serve_gui_connection(&mut b, &h, &*f, true));
        let reply = gui_command_on(
            a,
            &GuiCommand::Respond {
                request: 5,
                accept: true,
            },
        )
        .unwrap();
        assert_eq!(reply, GuiReply::Ok);
        assert_eq!(*fake.0.lock().unwrap(), vec![5]);

        let (a, mut b) = duplex();
        let (h, f) = (hub.clone(), fake.clone());
        std::thread::spawn(move || serve_gui_connection(&mut b, &h, &*f, true));
        assert_eq!(
            gui_command_on(a, &GuiCommand::Disconnect { session: 1 }).unwrap(),
            GuiReply::NotFound
        );
    }

    #[test]
    fn refused_client_gets_an_error() {
        let hub = GuiHub::new(1, String::new(), String::new());
        let fake = Fake::default();
        let (a, mut b) = duplex();
        let t = std::thread::spawn(move || gui_subscribe_on(a).map(|(s, _)| s));
        serve_gui_connection(&mut b, &hub, &fake, false).unwrap();
        drop(b);
        assert!(matches!(t.join().unwrap(), Err(HostError::IpcRefused(_))));
    }
}
