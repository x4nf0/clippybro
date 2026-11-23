//! Estado de la grabadora para la ventana principal, que corre en otro proceso: con qué codifica y por qué
//! no puede grabar. La bandeja lo escribe en recorder.json solo cuando cambia; la ventana lo relee al
//! enterarse de un cambio en la carpeta de la app.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::log::app_dir;
use crate::record::Device;

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
pub struct Status {
    /// Con qué está grabando (None: en pausa o todavía arrancando).
    pub device: Option<Device>,
    /// Por qué no puede grabar (se reintenta solo).
    pub error: Option<String>,
}

fn file() -> PathBuf {
    app_dir().join("recorder.json")
}

pub fn load() -> Status {
    std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(s: &Status) {
    let _ = std::fs::create_dir_all(app_dir());
    let _ = std::fs::write(file(), serde_json::to_string_pretty(s).unwrap_or_default());
}
