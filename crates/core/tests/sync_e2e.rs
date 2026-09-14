//! Ciclo de sync do client contra o server de verdade.
//!
//! Os testes unitários cobrem varredura, diff e plano isoladamente. Este
//! arquivo cobre o que só quebra na junta: dois devices, um sobe, o outro
//! recebe, e o baseline de cada um acompanha.
//!
//! Precisa do binário do server. Sem ele o teste é ignorado em vez de
//! falhar — quem roda `cargo test` sem ter buildado o server não fez nada
//! errado. Pra exercitar de fato:
//!
//! ```bash
//! cargo build -p save-sync-server && cargo test -p save-sync-core --test sync_e2e
//! ```

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

use save_sync_core::client::{sync_emulator, Baseline, ServerClient};

const SUBTREES: &[&str] = &["user/save"];

fn server_binary() -> Option<PathBuf> {
    // O test roda com CWD no crate; o target fica na raiz do workspace.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
    let bin = ["save-sync-server", "save-sync-server.exe"]
        .iter()
        .map(|name| root.join("target/debug").join(name))
        .find(|p| p.is_file())?;

    warn_if_stale(&bin, &root.join("apps/server/src"));
    Some(bin)
}

/// `cargo test -p save-sync-core` **não** rebuilda o binário do server — ele
/// não é dependência deste crate. Rodar contra um binário velho produz
/// falhas que parecem bug do client e não são; já aconteceu uma vez. O aviso
/// é ruidoso de propósito.
fn warn_if_stale(bin: &Path, src_dir: &Path) {
    let Ok(bin_time) = bin.metadata().and_then(|m| m.modified()) else {
        return;
    };
    let newest = std::fs::read_dir(src_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .max();

    if let Some(src_time) = newest {
        if src_time > bin_time {
            eprintln!(
                "\n!!! target/debug/save-sync-server está mais velho que apps/server/src.\n\
                 !!! Rode `cargo build -p save-sync-server` — este teste roda o binário,\n\
                 !!! e cargo test não o reconstrói sozinho.\n"
            );
        }
    }
}

struct Server {
    child: Child,
    base: String,
    _data: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Sobe um server numa porta própria, com dados descartáveis.
fn start_server(bin: &Path, port: u16) -> Server {
    let data = tempfile::tempdir().unwrap();

    let child = Command::new(bin)
        .env("SAVE_SYNC_ADDR", format!("127.0.0.1:{port}"))
        .env("SAVE_SYNC_DATA", data.path())
        .env("SAVE_SYNC_WEB", data.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("server não subiu");

    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Server { child, base, _data: data }
}

fn pairing_code(bin: &Path, data: &Path) -> String {
    let out = Command::new(bin)
        .arg("--pair")
        .env("SAVE_SYNC_DATA", data)
        .output()
        .expect("--pair falhou");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.rsplit(": ").next().map(|s| s.trim().to_string()))
        .expect("código não veio")
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

#[tokio::test]
async fn two_devices_converge_through_the_server() {
    let Some(bin) = server_binary() else {
        eprintln!("pulando: build o server com `cargo build -p save-sync-server`");
        return;
    };
    let server = start_server(&bin, 18801);
    let data = server._data.path().to_path_buf();

    // Dois devices pareados no mesmo server.
    let token_a = ServerClient::pair(&server.base, &pairing_code(&bin, &data), "pc", "windows")
        .await
        .expect("pareamento do device A")
        .device_token;
    let token_b = ServerClient::pair(&server.base, &pairing_code(&bin, &data), "celular", "android")
        .await
        .expect("pareamento do device B")
        .device_token;

    let a = ServerClient::new(&server.base, &token_a);
    let b = ServerClient::new(&server.base, &token_b);

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    // ─── A cria um save e sincroniza ────────────────────────────────────
    write(dir_a.path(), "user/save/jogo/a.sav", b"partida do device A");

    let report_a = sync_emulator(&a, "eden", dir_a.path(), SUBTREES, &Baseline::default())
        .await
        .expect("sync do A");
    assert_eq!(report_a.uploaded, 1);
    assert_eq!(report_a.downloaded, 0);
    assert_eq!(report_a.new_rev, 1);
    assert_eq!(report_a.baseline.last_rev, 1);
    assert!(report_a.baseline.files.contains_key("user/save/jogo/a.sav"));

    // ─── B, que nunca viu nada, recebe ──────────────────────────────────
    let report_b = sync_emulator(&b, "eden", dir_b.path(), SUBTREES, &Baseline::default())
        .await
        .expect("sync do B");
    assert_eq!(report_b.downloaded, 1);
    assert_eq!(report_b.uploaded, 0);
    assert_eq!(
        std::fs::read(dir_b.path().join("user/save/jogo/a.sav")).unwrap(),
        b"partida do device A",
        "o conteúdo tem que chegar idêntico"
    );

    // ─── Sync sem mudança não transfere nada ────────────────────────────
    let quieto = sync_emulator(&b, "eden", dir_b.path(), SUBTREES, &report_b.baseline)
        .await
        .expect("sync sem mudança");
    assert_eq!(quieto.uploaded, 0);
    assert_eq!(quieto.downloaded, 0);
    assert_eq!(quieto.deleted, 0);

    // ─── B edita, A recebe a edição ─────────────────────────────────────
    write(dir_b.path(), "user/save/jogo/a.sav", b"progresso novo do B");
    let editado = sync_emulator(&b, "eden", dir_b.path(), SUBTREES, &quieto.baseline)
        .await
        .expect("sync da edição");
    assert_eq!(editado.uploaded, 1);

    let recebido = sync_emulator(&a, "eden", dir_a.path(), SUBTREES, &report_a.baseline)
        .await
        .expect("A puxando a edição");
    assert_eq!(recebido.downloaded, 1);
    assert_eq!(
        std::fs::read(dir_a.path().join("user/save/jogo/a.sav")).unwrap(),
        b"progresso novo do B"
    );

    // ─── B apaga, e a deleção propaga pra A ─────────────────────────────
    std::fs::remove_file(dir_b.path().join("user/save/jogo/a.sav")).unwrap();
    let apagado = sync_emulator(&b, "eden", dir_b.path(), SUBTREES, &editado.baseline)
        .await
        .expect("sync da deleção");
    assert!(apagado.baseline.files.is_empty());

    let propagado = sync_emulator(&a, "eden", dir_a.path(), SUBTREES, &recebido.baseline)
        .await
        .expect("A aplicando a deleção");
    assert_eq!(propagado.deleted, 1);
    assert!(
        !dir_a.path().join("user/save/jogo/a.sav").exists(),
        "deleção tem que propagar, senão o arquivo ressuscita no próximo ciclo"
    );
}

#[tokio::test]
async fn simultaneous_edits_keep_both_versions() {
    let Some(bin) = server_binary() else {
        eprintln!("pulando: build o server com `cargo build -p save-sync-server`");
        return;
    };
    let server = start_server(&bin, 18802);
    let data = server._data.path().to_path_buf();

    let token_a = ServerClient::pair(&server.base, &pairing_code(&bin, &data), "pc", "windows")
        .await
        .unwrap()
        .device_token;
    let token_b = ServerClient::pair(&server.base, &pairing_code(&bin, &data), "celular", "android")
        .await
        .unwrap()
        .device_token;
    let a = ServerClient::new(&server.base, &token_a);
    let b = ServerClient::new(&server.base, &token_b);

    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();

    // Estado comum nos dois lados.
    write(dir_a.path(), "user/save/a.sav", b"comum");
    let base_a = sync_emulator(&a, "eden", dir_a.path(), SUBTREES, &Baseline::default())
        .await
        .unwrap()
        .baseline;
    let base_b = sync_emulator(&b, "eden", dir_b.path(), SUBTREES, &Baseline::default())
        .await
        .unwrap()
        .baseline;

    // Os dois editam o mesmo arquivo antes de qualquer sync.
    write(dir_a.path(), "user/save/a.sav", b"versao do A");
    write(dir_b.path(), "user/save/a.sav", b"versao do B");

    // A sincroniza primeiro e vence por não ter com quem competir.
    sync_emulator(&a, "eden", dir_a.path(), SUBTREES, &base_a)
        .await
        .unwrap();

    // B chega depois: agora há conflito de verdade.
    let conflito = sync_emulator(&b, "eden", dir_b.path(), SUBTREES, &base_b)
        .await
        .unwrap();
    assert_eq!(conflito.conflicts, 1, "edição dos dois lados é conflito");

    // A invariante que mais importa: nenhuma das duas versões some.
    let arquivos: Vec<String> = std::fs::read_dir(dir_b.path().join("user/save"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    let conteudos: Vec<Vec<u8>> = arquivos
        .iter()
        .map(|f| std::fs::read(dir_b.path().join("user/save").join(f)).unwrap())
        .collect();

    assert!(
        conteudos.iter().any(|c| c == b"versao do A"),
        "versão do A sumiu; arquivos: {arquivos:?}"
    );
    assert!(
        conteudos.iter().any(|c| c == b"versao do B"),
        "versão do B sumiu; arquivos: {arquivos:?}"
    );
    assert!(
        arquivos.iter().any(|f| f.contains("conflict")),
        "o perdedor deveria ter virado .conflictN; arquivos: {arquivos:?}"
    );
}
