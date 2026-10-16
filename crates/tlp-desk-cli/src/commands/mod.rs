//! Command-line structure: global options ([`Cli`]) and one module per subcommand.
//!
//! Adding a command (other lanes): create `commands/<name>.rs` with a `<Name>Args` clap
//! `Args` struct and a `run(ctx, args)` function, then add one line in each FEATURE HOOK below
//! (`mod`, the [`Cmd`] variant, the [`run`] arm). Dial/auth helpers live in [`common`].

pub mod chat;
pub mod clip_send;
pub mod common;
pub mod connect;
pub mod forward;
pub mod get_file;
pub mod help;
pub mod id;
pub mod info;
pub mod list;
pub mod ls;
pub mod mkdir;
pub mod mv;
pub mod remote_fs;
pub mod rm;
pub mod send_file;
pub mod serve;
pub mod set_password;
pub mod typing;
pub mod wol;
// FEATURE HOOK: `pub mod <command>;` for new subcommands.

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::output::Output;
use crate::text;
use common::Ctx;

/// The whole command line.
#[derive(Debug, Parser)]
#[command(name = "TLP-Desk-cli", version, about = text::ABOUT, after_help = crate::exit::HELP_TABLE)]
pub struct Cli {
    /// Directory holding the identity key and known hosts (default: the app's data dir).
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,
    /// More logging (repeat for even more). RUST_LOG overrides.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Language of human-readable output (JSON output is never translated).
    #[arg(
        long,
        global = true,
        value_enum,
        env = "TLP_DESK_LANG",
        default_value = "en"
    )]
    pub lang: text::Lang,
    /// Machine-readable output: one JSON object per line on stdout, each with a "type" field.
    #[arg(long, global = true)]
    pub json: bool,
    /// Only print results and errors, no progress lines.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print this device's ID and key fingerprint.
    Id(id::IdArgs),
    /// Discover TLP-Desk devices on the LAN.
    List(list::ListArgs),
    /// Host: let LAN viewers see and control this PC.
    Serve(serve::ServeArgs),
    /// Viewer: connect to a host and report fps / bitrate / RTT every second.
    Connect(connect::ConnectArgs),
    /// Viewer: show a host's OS, CPU, RAM, disks, uptime and name (needs VIEW).
    Info(info::InfoArgs),
    /// Wake a sleeping PC on the LAN (Wake-on-LAN) by MAC address or alias.
    #[command(visible_alias = "exec-wol")]
    Wol(wol::WolArgs),
    /// Viewer: forward local TCP ports to services on (or next to) the host until Ctrl+C,
    /// e.g. `forward 192.168.1.20 8080:127.0.0.1:80`. The host must allow port forwarding.
    Forward(forward::ForwardArgs),
    /// Viewer: send one chat message to a host and print the replies.
    Chat(chat::ChatArgs),
    /// Viewer: put text on the host's clipboard (this PC's clipboard is not touched).
    ClipSend(clip_send::ClipSendArgs),
    /// List a folder on the remote PC (or its drives and known folders). Needs "files".
    Ls(ls::LsArgs),
    /// Create a folder on the remote PC. Needs "files".
    Mkdir(mkdir::MkdirArgs),
    /// Delete a file or folder on the remote PC (permanently). Needs "files".
    Rm(rm::RmArgs),
    /// Rename or move within one drive of the remote PC. Needs "files".
    Mv(mv::MvArgs),
    /// Viewer: send local files and folders to a host (--to <host folder>).
    SendFile(send_file::SendFileArgs),
    /// Viewer: download files and folders from a host (--to <local folder>).
    GetFile(get_file::GetFileArgs),
    /// Host: set the permanent unattended-access password from the TLP_DESK_PASSWORD
    /// environment variable, or remove it with --remove.
    SetPassword(set_password::SetPasswordArgs),
    // FEATURE HOOK: one variant per new subcommand.
}

impl Cli {
    /// The output settings these flags select.
    pub fn output(&self) -> Output {
        Output {
            json: self.json,
            quiet: self.quiet,
        }
    }
}

/// Runs the selected subcommand.
pub async fn run(cli: &Cli) -> Result<()> {
    let ctx = Ctx {
        data_dir: cli
            .data_dir
            .clone()
            .unwrap_or_else(tlp_desk_crypto::default_dir),
        out: cli.output(),
    };
    match &cli.cmd {
        Cmd::Id(a) => id::run(&ctx, a),
        Cmd::List(a) => list::run(&ctx, a).await,
        Cmd::Serve(a) => serve::run(&ctx, a).await,
        Cmd::Connect(a) => connect::run(&ctx, a).await,
        Cmd::Info(a) => info::run(&ctx, a).await,
        Cmd::Wol(a) => wol::run(&ctx, a),
        Cmd::Forward(a) => forward::run(&ctx, a).await,
        Cmd::Chat(a) => chat::run(&ctx, a).await,
        Cmd::ClipSend(a) => clip_send::run(&ctx, a).await,
        Cmd::Ls(a) => ls::run(&ctx, a).await,
        Cmd::Mkdir(a) => mkdir::run(&ctx, a).await,
        Cmd::Rm(a) => rm::run(&ctx, a).await,
        Cmd::Mv(a) => mv::run(&ctx, a).await,
        Cmd::SendFile(a) => send_file::run(&ctx, a).await,
        Cmd::GetFile(a) => get_file::run(&ctx, a).await,
        Cmd::SetPassword(a) => set_password::run(&ctx, a),
        // FEATURE HOOK: one arm per new subcommand.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("TLP-Desk-cli").chain(args.iter().copied()))
    }

    #[test]
    fn clap_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn id_and_list_parse() {
        assert!(matches!(parse(&["id"]).unwrap().cmd, Cmd::Id(_)));
        let Cmd::List(l) = parse(&["list"]).unwrap().cmd else {
            panic!("not list")
        };
        assert_eq!(l.seconds, 5);
        let Cmd::List(l) = parse(&["list", "--seconds", "2"]).unwrap().cmd else {
            panic!("not list")
        };
        assert_eq!(l.seconds, 2);
    }

    #[test]
    fn connect_parses_target_auth_and_options() {
        let cli = parse(&[
            "connect",
            "123456789@192.168.1.5:47400",
            "--code",
            "123456",
            "--seconds",
            "3",
            "--monitor",
            "1",
            "--forget-key",
            "--data-dir",
            "C:\\بيانات",
        ])
        .unwrap();
        assert_eq!(cli.data_dir, Some(PathBuf::from("C:\\بيانات")));
        let Cmd::Connect(c) = cli.cmd else {
            panic!("not connect")
        };
        assert_eq!(c.dial.target, "123456789@192.168.1.5:47400");
        assert_eq!(c.dial.auth.code.as_deref(), Some("123456"));
        assert!(!c.dial.auth.password);
        assert!(c.dial.forget_key);
        assert_eq!(c.dial.port, 0);
        assert_eq!((c.seconds, c.monitor), (3, Some(1)));
    }

    #[test]
    fn password_is_never_taken_from_argv() {
        // `--password` is a flag; a value after it is an unexpected extra argument.
        assert!(parse(&["connect", "10.0.0.2", "--password", "secret"]).is_err());
        assert!(parse(&["connect", "10.0.0.2", "--password=secret"]).is_err());
        let Cmd::Connect(c) = parse(&["connect", "10.0.0.2", "--password"]).unwrap().cmd else {
            panic!("not connect")
        };
        assert!(c.dial.auth.password);
    }

    #[test]
    fn serve_flags_map_to_host_config() {
        let Cmd::Serve(s) = parse(&["serve", "--code", "--view-only", "--name", "حاسوب المكتب"])
            .unwrap()
            .cmd
        else {
            panic!("not serve")
        };
        let cfg = s.host_config();
        assert!(cfg.allow_one_time_code);
        assert_eq!(cfg.default_permissions, tlp_desk_proto::Permissions::VIEW);
        assert_eq!(cfg.device_name, "حاسوب المكتب");
        assert_eq!(s.port, tlp_desk_net::DEFAULT_PORT);
    }

    #[test]
    fn clip_send_parses_text_or_file() {
        let Cmd::ClipSend(c) =
            parse(&["clip-send", "10.0.0.2", "مرحبا\u{200F}", "--code", "123456"])
                .unwrap()
                .cmd
        else {
            panic!("not clip-send")
        };
        assert_eq!(c.dial.target, "10.0.0.2");
        assert_eq!(c.text.as_deref(), Some("مرحبا\u{200F}"));
        assert_eq!(c.dial.auth.code.as_deref(), Some("123456"));
        let Cmd::ClipSend(c) = parse(&["clip-send", "10.0.0.2", "--file", "نص.txt"])
            .unwrap()
            .cmd
        else {
            panic!("not clip-send")
        };
        assert_eq!(c.file, Some(PathBuf::from("نص.txt")));
        assert!(parse(&["clip-send", "10.0.0.2", "x", "--file", "a.txt"]).is_err());
        let Cmd::Serve(s) = parse(&["serve", "--clipboard"]).unwrap().cmd else {
            panic!("not serve")
        };
        let cfg = s.host_config();
        assert!(cfg
            .default_permissions
            .contains(tlp_desk_proto::Permissions::CLIPBOARD));
        assert!(cfg.features.names().contains(&"clipboard"));
    }

    #[test]
    fn lang_is_global_and_validated() {
        assert_eq!(parse(&["--lang", "ar", "id"]).unwrap().lang, text::Lang::Ar);
        assert_eq!(parse(&["id", "--lang", "en"]).unwrap().lang, text::Lang::En);
        assert!(parse(&["--lang", "fr", "id"]).is_err());
    }

    #[test]
    fn json_and_quiet_are_global() {
        let cli = parse(&["list", "--json", "-q"]).unwrap();
        assert_eq!(
            cli.output(),
            Output {
                json: true,
                quiet: true
            }
        );
        assert_eq!(parse(&["id"]).unwrap().output(), Output::default());
    }

    #[test]
    fn help_documents_exit_codes() {
        let help = Cli::command().render_long_help().to_string();
        assert!(help.contains("Exit codes:"));
        assert!(help.contains("TLP_DESK_PASSWORD"));
    }

    #[test]
    fn info_parses_target_auth_and_timeout() {
        let Cmd::Info(i) = parse(&[
            "--json",
            "info",
            "مكتب-المحاسبة",
            "--code",
            "654321",
            "--timeout",
            "3",
        ])
        .unwrap()
        .cmd
        else {
            panic!("not info")
        };
        assert_eq!(i.dial.target, "مكتب-المحاسبة");
        assert_eq!(i.dial.auth.code.as_deref(), Some("654321"));
        assert_eq!(i.timeout, 3);
        assert!(parse(&["info"]).is_err());
    }

    #[test]
    fn wol_and_its_alias_parse() {
        for name in ["wol", "exec-wol"] {
            let Cmd::Wol(w) = parse(&[name, "01:23:45:67:89:ab", "--to", "192.168.1.255"])
                .unwrap()
                .cmd
            else {
                panic!("not wol")
            };
            assert_eq!(w.target, "01:23:45:67:89:ab");
            assert_eq!(w.to.as_deref(), Some("192.168.1.255"));
        }
    }

    #[test]
    fn file_commands_parse_with_arabic_paths() {
        let Cmd::Ls(l) = parse(&["ls", "10.0.0.2", "C:\\Users\\مستخدم", "--code", "123456"])
            .unwrap()
            .cmd
        else {
            panic!("not ls")
        };
        assert_eq!(l.dial.target, "10.0.0.2");
        assert_eq!(l.path.as_deref(), Some("C:\\Users\\مستخدم"));
        assert_eq!(l.dial.auth.code.as_deref(), Some("123456"));
        let Cmd::Ls(l) = parse(&["ls", "10.0.0.2"]).unwrap().cmd else {
            panic!("not ls")
        };
        assert_eq!(l.path, None);
        let Cmd::Mkdir(m) = parse(&["mkdir", "10.0.0.2", "D:\\مجلد جديد"]).unwrap().cmd
        else {
            panic!("not mkdir")
        };
        assert_eq!(m.path, "D:\\مجلد جديد");
        let Cmd::Rm(r) = parse(&["rm", "10.0.0.2", "D:\\x", "-r"]).unwrap().cmd else {
            panic!("not rm")
        };
        assert!(r.recursive);
        let Cmd::Mv(m) = parse(&["mv", "10.0.0.2", "D:\\a.txt", "D:\\ب.txt"])
            .unwrap()
            .cmd
        else {
            panic!("not mv")
        };
        assert_eq!((m.from.as_str(), m.to.as_str()), ("D:\\a.txt", "D:\\ب.txt"));
        assert!(parse(&["mv", "10.0.0.2", "D:\\a.txt"]).is_err());
        let Cmd::Serve(s) = parse(&["serve", "--allow-files"]).unwrap().cmd else {
            panic!("not serve")
        };
        assert!(s
            .host_config()
            .default_permissions
            .contains(tlp_desk_proto::Permissions::FILES));
    }

    #[test]
    fn file_errors_map_to_exit_codes_and_messages() {
        use tlp_desk_files::fs::FsClientError;
        use tlp_desk_proto::FsError;
        let e = anyhow::Error::from(FsClientError::NoPermission);
        assert_eq!(
            crate::exit::Exit::of(&e),
            crate::exit::Exit::PermissionDenied
        );
        let e = anyhow::Error::from(FsClientError::Timeout);
        assert_eq!(crate::exit::Exit::of(&e), crate::exit::Exit::Timeout);
        let e = anyhow::Error::from(FsClientError::Remote(FsError::NotFound));
        assert_eq!(crate::exit::Exit::of(&e), crate::exit::Exit::Failure);
        let ar = text::with_lang(text::Lang::Ar, || text::error_line(&e));
        assert!(ar.contains("غير موجود"), "{ar}");
    }

    #[test]
    fn every_subcommand_parses_and_is_listed_here() {
        // One valid command line per subcommand; a new command must add its row (so every
        // lane's command is parse-tested and appears in the Arabic help too).
        let samples: &[&[&str]] = &[
            &["id"],
            &["list", "--json"],
            &["serve", "--name", "مكتب"],
            &["connect", "123456789", "--code", "042917"],
            &["info", "123456789", "--code", "042917"],
            &["set-password", "--remove"],
            &["wol", "01:23:45:67:89:ab"],
            &["exec-wol", "مكتب"],
            &[
                "forward",
                "10.0.0.2",
                "8080:127.0.0.1:80",
                "--code",
                "123456",
            ],
            &["chat", "10.0.0.2", "مرحبا", "--code", "123456"],
            &["clip-send", "10.0.0.2", "نص", "--code", "123456"],
            &["ls", "10.0.0.2", "--code", "123456"],
            &["mkdir", "10.0.0.2", "D:/x", "--code", "123456"],
            &["rm", "10.0.0.2", "D:/x", "--code", "123456"],
            &["mv", "10.0.0.2", "D:/a", "D:/b", "--code", "123456"],
            &[
                "send-file",
                "10.0.0.2",
                "a.txt",
                "--to",
                "D:/",
                "--code",
                "123456",
            ],
            &["get-file", "10.0.0.2", "D:/a.txt", "--code", "123456"],
        ];
        let mut covered = std::collections::BTreeSet::new();
        for argv in samples {
            let cli = parse(argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            let _ = cli;
            let name = if argv[0] == "exec-wol" {
                "wol"
            } else {
                argv[0]
            };
            covered.insert(name);
        }
        for sub in Cli::command().get_subcommands() {
            assert!(
                covered.contains(sub.get_name()),
                "add a sample command line for `{}`",
                sub.get_name()
            );
        }
    }

    #[test]
    fn password_flag_is_rejected_for_every_dialing_command() {
        for cmd in ["connect", "info", "chat", "ls"] {
            let r = parse(&[cmd, "10.0.0.2", "--password=hunter2"]);
            assert!(r.is_err(), "{cmd} must not take --password");
        }
    }

    #[test]
    fn unknown_command_and_missing_target_are_errors() {
        assert!(parse(&["frobnicate"]).is_err());
        assert!(parse(&["connect"]).is_err());
        assert!(parse(&[]).is_err());
    }
}
