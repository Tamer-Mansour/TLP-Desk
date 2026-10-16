//! Arabic `--help`: clap's derive bakes the English doc comments in at compile time, so for
//! `--lang ar` we swap the summaries on the built [`clap::Command`] before parsing.
//!
//! [`ARABIC`] must have one row per subcommand (a test enforces it), so a lane that adds a
//! command also adds its Arabic summary here.

use clap::{Command, CommandFactory};

use super::Cli;
use crate::text::Lang;

/// Arabic summary of the whole tool.
pub const ABOUT_AR: &str =
    "عميل TLP-Desk بلا واجهة للنصوص البرمجية والوكلاء الآليين (على الشبكة المحلية فقط)";

/// Arabic table of the exit codes (same numbers as `--help` in English).
pub const EXIT_TABLE_AR: &str = "\
رموز الخروج:
  0    ok                 نُفّذ الأمر كما طُلب
  1    failure            أي خطأ آخر (إدخال/إخراج، ملف تالف، داخلي)
  2    usage              سطر أوامر خاطئ
  3    not_found          لم يُعثر على الجهاز أو الاسم المستعار في الشبكة
  4    unreachable        المضيف غير متاح أو خارج القائمة المسموحة
  5    auth_rejected      رُفض الاتصال أو الرمز/كلمة المرور خاطئة أو الحساب مقفل
  6    key_mismatch       مفتاح المضيف لا يطابق (احتمال هجوم وسيط)
  7    permission_denied  الجلسة لا تملك الصلاحية التي يحتاجها الأمر
  8    timeout            لا رد في الوقت المحدد
  9    version_mismatch   الطرف الآخر يستخدم إصداراً آخر من البروتوكول
  130  interrupted        Ctrl+C

متغيرات البيئة:
  TLP_DESK_PASSWORD  كلمة مرور الوصول بلا حضور (لا تُكتب أبداً في سطر الأوامر)
  TLP_DESK_LANG      القيمة الافتراضية لخيار --lang (en أو ar)";

/// `(subcommand name, Arabic summary)`.
pub const ARABIC: &[(&str, &str)] = &[
    ("id", "اطبع معرّف هذا الجهاز وبصمة مفتاحه."),
    ("list", "اكتشف أجهزة TLP-Desk على الشبكة المحلية."),
    (
        "serve",
        "مضيف: اسمح لمشاهدين على الشبكة برؤية هذا الحاسوب والتحكم به.",
    ),
    (
        "connect",
        "مشاهد: اتصل بمضيف واعرض الإطارات والبيانات وزمن الاستجابة كل ثانية.",
    ),
    (
        "info",
        "مشاهد: اعرض نظام المضيف ومعالجه وذاكرته وأقراصه ومدة تشغيله واسمه (يحتاج صلاحية العرض).",
    ),
    (
        "wol",
        "أيقظ حاسوباً نائماً على الشبكة (Wake-on-LAN) بعنوان MAC أو اسم مستعار.",
    ),
    (
        "forward",
        "مشاهد: وجّه منافذ TCP محلية إلى خدمات على المضيف حتى Ctrl+C. يجب أن يسمح المضيف بذلك.",
    ),
    (
        "chat",
        "مشاهد: أرسل رسالة محادثة واحدة إلى مضيف واطبع الردود.",
    ),
    (
        "clip-send",
        "مشاهد: ضع نصاً في حافظة المضيف (حافظة هذا الحاسوب لا تتغير).",
    ),
    (
        "ls",
        "اعرض محتوى مجلد على الحاسوب البعيد أو أقراصه ومجلداته المعروفة (يحتاج صلاحية الملفات).",
    ),
    (
        "mkdir",
        "أنشئ مجلداً على الحاسوب البعيد (يحتاج صلاحية الملفات).",
    ),
    (
        "rm",
        "احذف ملفاً أو مجلداً على الحاسوب البعيد نهائياً (يحتاج صلاحية الملفات).",
    ),
    (
        "mv",
        "أعد تسمية عنصر أو انقله داخل القرص نفسه على الحاسوب البعيد (يحتاج صلاحية الملفات).",
    ),
    (
        "send-file",
        "مشاهد: أرسل ملفات ومجلدات محلية إلى مضيف (--to مجلد المضيف).",
    ),
    (
        "get-file",
        "مشاهد: نزّل ملفات ومجلدات من مضيف (--to مجلد محلي).",
    ),
];

/// The command with Arabic summaries when `lang` is Arabic, plain otherwise.
pub fn command(lang: Lang) -> Command {
    let cmd = Cli::command();
    if lang != Lang::Ar {
        return cmd;
    }
    let names: Vec<String> = cmd
        .get_subcommands()
        .map(|s| s.get_name().to_owned())
        .collect();
    let mut cmd = cmd.about(ABOUT_AR).after_help(EXIT_TABLE_AR);
    for name in names {
        if let Some((_, ar)) = ARABIC.iter().find(|(n, _)| *n == name) {
            cmd = cmd.mut_subcommand(name, |s| s.about(*ar));
        }
    }
    cmd
}

/// The language asked for on the command line or in `TLP_DESK_LANG`, found before clap runs.
pub fn requested_lang(args: &[String], env: Option<&str>) -> Lang {
    let mut found = env;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if a == "--lang" {
            found = it.next().map(String::as_str);
        } else if let Some(v) = a.strip_prefix("--lang=") {
            found = Some(v);
        }
    }
    if found == Some("ar") {
        Lang::Ar
    } else {
        Lang::En
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_subcommand_has_an_arabic_summary() {
        for sub in Cli::command().get_subcommands() {
            assert!(
                ARABIC.iter().any(|(n, _)| *n == sub.get_name()),
                "no Arabic summary for `{}`: add it to commands/help.rs",
                sub.get_name()
            );
        }
        assert_eq!(ARABIC.len(), Cli::command().get_subcommands().count());
    }

    #[test]
    fn arabic_help_shows_arabic_summaries_and_exit_codes() {
        let help = command(Lang::Ar).render_long_help().to_string();
        assert!(help.contains("أيقظ حاسوباً"), "{help}");
        assert!(help.contains("رموز الخروج"));
        assert!(help.contains("TLP_DESK_PASSWORD"));
        let en = command(Lang::En).render_long_help().to_string();
        assert!(en.contains("Wake a sleeping PC"));
        assert!(en.contains("Exit codes:"));
    }

    #[test]
    fn exit_tables_list_the_same_codes() {
        for e in crate::exit::Exit::ALL {
            let row = format!("  {:<4} {}", e.code(), e.name());
            assert!(EXIT_TABLE_AR.contains(&row), "missing {row:?}");
        }
    }

    #[test]
    fn requested_lang_reads_flag_then_env() {
        let a = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(requested_lang(&a(&["x", "--lang", "ar"]), None), Lang::Ar);
        assert_eq!(
            requested_lang(&a(&["x", "--lang=ar", "id"]), None),
            Lang::Ar
        );
        assert_eq!(requested_lang(&a(&["x", "id"]), Some("ar")), Lang::Ar);
        assert_eq!(
            requested_lang(&a(&["x", "--lang", "en"]), Some("ar")),
            Lang::En
        );
        assert_eq!(requested_lang(&a(&["x", "--lang"]), None), Lang::En);
    }
}
