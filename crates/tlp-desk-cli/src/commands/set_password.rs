//! `set-password`: set or remove the permanent unattended-access password.
//!
//! The password comes from the `TLP_DESK_PASSWORD` environment variable, never from argv, so it
//! stays out of process lists and shell history. Only the Argon2id verifier is stored.

use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::json;
use tlp_desk_crypto::{password_strength, PasswordStore, PasswordStrength, PasswordVerifier};

use super::common::{load_identity, Ctx};
use crate::text;

/// Arguments of `set-password`.
#[derive(Debug, Clone, Default, Args)]
pub struct SetPasswordArgs {
    /// Remove the password (password sign-in off).
    #[arg(long)]
    pub remove: bool,
}

/// Runs `set-password`.
pub fn run(ctx: &Ctx, args: &SetPasswordArgs) -> Result<()> {
    let store = PasswordStore::new(&ctx.data_dir);
    if args.remove {
        store.clear().context("cannot remove the password")?;
        ctx.out.result(
            text::password_removed(),
            &json!({ "type": "password_removed" }),
        );
        return Ok(());
    }
    let password = std::env::var("TLP_DESK_PASSWORD")
        .context("set-password needs the TLP_DESK_PASSWORD environment variable")?;
    if password_strength(&password) == PasswordStrength::TooShort {
        bail!(text::password_too_short());
    }
    let identity = load_identity(&ctx.data_dir)?;
    let verifier = PasswordVerifier::derive(&password, &identity.fingerprint())?;
    store
        .save(&verifier)
        .with_context(|| format!("cannot write {}", store.path().display()))?;
    let path = store.path().display().to_string();
    ctx.out.result(
        text::password_set(&path),
        &json!({ "type": "password_set", "path": path }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn remove_flag_parses_and_password_is_not_an_argument() {
        #[derive(Parser)]
        struct P {
            #[command(flatten)]
            a: SetPasswordArgs,
        }
        assert!(P::try_parse_from(["x", "--remove"]).unwrap().a.remove);
        assert!(P::try_parse_from(["x", "--password", "secret"]).is_err());
    }

    #[test]
    fn removing_with_no_password_set_is_fine() {
        let dir = std::env::temp_dir().join(format!("tlp-cli-pw-كلمة-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = Ctx {
            data_dir: dir.clone(),
            out: crate::output::Output {
                json: true,
                quiet: true,
            },
        };
        run(&ctx, &SetPasswordArgs { remove: true }).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
