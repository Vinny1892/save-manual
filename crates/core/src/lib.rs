//! Núcleo compartilhado entre o client de PC (Tauri), o server e os clients
//! que vierem. Nada aqui conhece Tauri, HTTP ou UI — só o domínio:
//! detecção de paths de emulador, leitura/parsing de saves, destinos de
//! backup e a ponte com o rclone.
//!
//! A regra que mantém isso honesto: se um módulo daqui precisar de
//! `AppHandle`, de um router ou de qualquer coisa de transporte, ele está no
//! lugar errado — o acoplamento vai pra camada de cima (`apps/`).

pub mod backend;
pub mod client;
pub mod db;
pub mod detect;
pub mod engine;
pub mod history;
pub mod protocol;
pub mod ps2db;
pub mod ps2mc;
pub mod rclone;
pub mod saves;
pub mod sync;
pub mod titledb;
