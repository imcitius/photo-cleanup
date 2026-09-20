//! Which language the user-facing text is in.
//!
//! Two kinds of text come out of this workspace and they have different
//! needs. The command line is read by whoever is typing, and one language —
//! English — is enough. The web interface is read by whoever owns the
//! archive, and that is a setting they choose.
//!
//! Both kinds are produced by the same crates: a refusal reason from
//! `pc-apply` shows up in a terminal and in a browser. So the language is a
//! property of the running process, set once by the binary that owns it: the
//! CLI pins it to English, the server sets it from the settings table.
//!
//! The two texts are written side by side at the call site, with the `tr!`
//! and `tf!` macros. Keeping them together is deliberate: a table of keys
//! somewhere else is a table that drifts, and a missing key is discovered by
//! a user rather than by the compiler.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    Ru,
    #[default]
    En,
}

impl Lang {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ru => "ru",
            Self::En => "en",
        }
    }

    /// Anything unrecognised is English, which is also the default.
    pub fn parse(s: &str) -> Self {
        match s {
            "ru" => Self::Ru,
            _ => Self::En,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(1);

pub fn set(lang: Lang) {
    CURRENT.store(lang as u8, Ordering::Relaxed);
}

pub fn current() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        0 => Lang::Ru,
        _ => Lang::En,
    }
}

/// Pick between two fixed strings.
#[macro_export]
macro_rules! tr {
    ($ru:expr, $en:expr $(,)?) => {
        match $crate::lang::current() {
            $crate::lang::Lang::Ru => $ru,
            $crate::lang::Lang::En => $en,
        }
    };
}

/// Pick between two format strings and fill them.
///
/// Both take the same arguments, but not necessarily in the same order —
/// each side is an ordinary `format!`, so either may name them positionally.
#[macro_export]
macro_rules! tf {
    ($ru:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        match $crate::lang::current() {
            $crate::lang::Lang::Ru => format!($ru $(, $arg)*),
            $crate::lang::Lang::En => format!($en $(, $arg)*),
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One process, one language: the tests share it, so they set it.
    fn with(lang: Lang, f: impl FnOnce()) {
        let before = current();
        set(lang);
        f();
        set(before);
    }

    #[test]
    fn english_is_the_default_and_the_fallback() {
        assert_eq!(Lang::default(), Lang::En);
        assert_eq!(Lang::parse("ru"), Lang::Ru);
        assert_eq!(Lang::parse("de"), Lang::En);
        assert_eq!(Lang::parse(""), Lang::En);
    }

    #[test]
    fn fixed_strings_follow_the_setting() {
        with(Lang::Ru, || assert_eq!(tr!("да", "yes"), "да"));
        with(Lang::En, || assert_eq!(tr!("да", "yes"), "yes"));
    }

    #[test]
    fn formatted_strings_take_the_same_arguments_in_either_order() {
        with(Lang::Ru, || {
            assert_eq!(
                tf!("осталось {0} из {1}", "{0} of {1} left", 3, 9),
                "осталось 3 из 9"
            );
        });
        with(Lang::En, || {
            assert_eq!(
                tf!("осталось {0} из {1}", "{0} of {1} left", 3, 9),
                "3 of 9 left"
            );
        });
    }
}
