//! Idioma de la interfaz: español, inglés o portugués. En "Automático" (el de fábrica) sigue el
//! idioma de Windows; si no es ninguno de los tres, inglés.
//!
//! Cada texto se escribe una sola vez, en el lugar donde se usa, con sus tres versiones:
//! `tr!("Guardar", "Save", "Salvar")` y, si lleva valores, `trf!("Hace {n} min", "{n} min ago", "Há {n} min")`.
//! El idioma elegido lo guarda config.json y cada proceso lo aplica al leerlo (`config::load`).

use std::sync::atomic::{AtomicU8, Ordering};

use serde::{Deserialize, Serialize};

/// Idioma con que se dibuja la interfaz.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Es,
    En,
    Pt,
}

impl Lang {
    pub const ALL: [Lang; 3] = [Lang::Es, Lang::En, Lang::Pt];

    /// Nombre del idioma en ese mismo idioma (así lo encuentra quien no entiende el actual).
    pub fn name(self) -> &'static str {
        match self {
            Lang::Es => "Español",
            Lang::En => "English",
            Lang::Pt => "Português",
        }
    }

    /// El idioma de Windows del usuario (el de los menús), si es uno de los que hay.
    fn system() -> Lang {
        use windows::Win32::Globalization::GetUserDefaultUILanguage;
        // Los 10 bits de abajo del LANGID son el idioma principal, sin la variante regional.
        match unsafe { GetUserDefaultUILanguage() } & 0x3FF {
            0x0A => Lang::Es,
            0x16 => Lang::Pt,
            _ => Lang::En,
        }
    }
}

/// Lo que se elige en Ajustes. En config.json: "auto", "es", "en" o "pt".
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    Auto,
    Es,
    En,
    Pt,
}

impl Language {
    pub fn resolve(self) -> Lang {
        match self {
            Language::Auto => Lang::system(),
            Language::Es => Lang::Es,
            Language::En => Lang::En,
            Language::Pt => Lang::Pt,
        }
    }
}

impl From<Lang> for Language {
    fn from(l: Lang) -> Self {
        match l {
            Lang::Es => Language::Es,
            Lang::En => Language::En,
            Lang::Pt => Language::Pt,
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(Lang::Es as u8);

pub fn set(choice: Language) {
    CURRENT.store(choice.resolve() as u8, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::En,
        2 => Lang::Pt,
        _ => Lang::Es,
    }
}

/// La versión del idioma actual. Usá `tr!`.
pub fn pick<T>(es: T, en: T, pt: T) -> T {
    match lang() {
        Lang::Es => es,
        Lang::En => en,
        Lang::Pt => pt,
    }
}

/// Texto fijo en los tres idiomas: `tr!(español, inglés, portugués)`.
macro_rules! tr {
    ($es:expr, $en:expr, $pt:expr $(,)?) => {
        $crate::i18n::pick($es, $en, $pt)
    };
}

/// Texto con valores en los tres idiomas, como `format!`: `trf!("es {x}", "en {x}", "pt {x}")`.
macro_rules! trf {
    ($es:literal, $en:literal, $pt:literal $(, $arg:expr)* $(,)?) => {
        match $crate::i18n::lang() {
            $crate::i18n::Lang::Es => format!($es $(, $arg)*),
            $crate::i18n::Lang::En => format!($en $(, $arg)*),
            $crate::i18n::Lang::Pt => format!($pt $(, $arg)*),
        }
    };
}

pub(crate) use {tr, trf};
