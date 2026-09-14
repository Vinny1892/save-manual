package net.savesync.protocol

import java.io.File
import kotlinx.serialization.encodeToString
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * O client Kotlin contra o server Rust de verdade.
 *
 * É o teste que justifica este módulo existir separado do app: o risco que
 * custa caro aqui é o wire format divergir entre as duas linguagens, e isso
 * só aparece com os dois rodando. Nada disto precisa de aparelho Android —
 * é JVM puro contra `target/debug/save-sync-server`.
 *
 * Sem o binário, os testes passam sem exercitar nada em vez de falhar: quem
 * roda `./gradlew test` sem ter buildado o server não fez nada errado.
 */
class SyncE2ETest {

    private val repoRoot: File = File(System.getProperty("user.dir"))
        .parentFile   // apps/android
        .parentFile   // apps
        .parentFile   // raiz do repo

    private val serverBinary: File? =
        listOf("save-sync-server.exe", "save-sync-server")
            .map { File(repoRoot, "target/debug/$it") }
            .firstOrNull { it.isFile }

    private var process: Process? = null

    @AfterTest
    fun stopServer() {
        process?.destroyForcibly()
        process = null
    }

    private fun startServer(port: Int, dataDir: File): String {
        val bin = serverBinary!!
        process = ProcessBuilder(bin.absolutePath)
            .apply {
                environment()["SAVE_SYNC_ADDR"] = "127.0.0.1:$port"
                environment()["SAVE_SYNC_DATA"] = dataDir.absolutePath
                environment()["SAVE_SYNC_WEB"] = dataDir.absolutePath
            }
            .redirectOutput(ProcessBuilder.Redirect.DISCARD)
            .redirectError(ProcessBuilder.Redirect.DISCARD)
            .start()

        val base = "http://127.0.0.1:$port"
        repeat(100) {
            runCatching {
                java.net.Socket("127.0.0.1", port).close()
                return base
            }
            Thread.sleep(100)
        }
        return base
    }

    private fun pairingCode(dataDir: File): String {
        val out = ProcessBuilder(serverBinary!!.absolutePath, "--pair")
            .apply { environment()["SAVE_SYNC_DATA"] = dataDir.absolutePath }
            .start()
        val text = out.inputStream.bufferedReader().readText()
        out.waitFor()
        return text.lineSequence().first().substringAfterLast(": ").trim()
    }

    private fun tempDir(name: String): File =
        File(System.getProperty("java.io.tmpdir"), "save-sync-$name-${System.nanoTime()}")
            .apply { mkdirs(); deleteOnExit() }

    private fun write(root: File, rel: String, content: String) {
        val f = File(root, rel)
        f.parentFile.mkdirs()
        f.writeText(content)
    }

    @Test
    fun `kotlin client syncs against the rust server`() {
        if (serverBinary == null) {
            println("pulando: build o server com `cargo build -p save-sync-server`")
            return
        }

        val data = tempDir("srv")
        val base = startServer(18901, data)

        val tokenA = SyncClient.pair(base, pairingCode(data), "celular").deviceToken
        val tokenB = SyncClient.pair(base, pairingCode(data), "pc", "windows").deviceToken
        val a = SyncClient(base, tokenA)
        val b = SyncClient(base, tokenB)

        val dirA = tempDir("a")
        val dirB = tempDir("b")
        val storageA = LocalFileStorage(dirA)
        val storageB = LocalFileStorage(dirB)

        // ─── A cria um save e sobe ──────────────────────────────────────
        write(dirA, "user/save/jogo/a.sav", "partida do Android")

        val reportA = syncEmulator(a, "eden", storageA, Baseline())
        assertEquals(1, reportA.uploaded, "o save novo tem que subir")
        assertEquals(1L, reportA.newRev)

        // ─── B recebe ───────────────────────────────────────────────────
        val reportB = syncEmulator(b, "eden", storageB, Baseline())
        assertEquals(1, reportB.downloaded)
        assertEquals(
            "partida do Android",
            File(dirB, "user/save/jogo/a.sav").readText(),
            "o conteúdo tem que atravessar as duas linguagens intacto",
        )

        // ─── Sync sem mudança não transfere ─────────────────────────────
        val quiet = syncEmulator(b, "eden", storageB, reportB.baseline)
        assertEquals(0, quiet.uploaded)
        assertEquals(0, quiet.downloaded)

        // ─── Deleção propaga ────────────────────────────────────────────
        File(dirB, "user/save/jogo/a.sav").delete()
        syncEmulator(b, "eden", storageB, quiet.baseline)

        val propagated = syncEmulator(a, "eden", storageA, reportA.baseline)
        assertEquals(1, propagated.deleted)
        assertTrue(
            !File(dirA, "user/save/jogo/a.sav").exists(),
            "deleção tem que propagar, senão o arquivo ressuscita",
        )
    }

    @Test
    fun `conflict keeps both versions`() {
        if (serverBinary == null) {
            println("pulando: build o server com `cargo build -p save-sync-server`")
            return
        }

        val data = tempDir("srv2")
        val base = startServer(18902, data)

        val a = SyncClient(base, SyncClient.pair(base, pairingCode(data), "celular").deviceToken)
        val b = SyncClient(base, SyncClient.pair(base, pairingCode(data), "pc", "windows").deviceToken)

        val dirA = tempDir("ca")
        val dirB = tempDir("cb")
        val storageA = LocalFileStorage(dirA)
        val storageB = LocalFileStorage(dirB)

        write(dirA, "user/save/a.sav", "comum")
        val baseA = syncEmulator(a, "eden", storageA, Baseline()).baseline
        val baseB = syncEmulator(b, "eden", storageB, Baseline()).baseline

        // Os dois editam antes de qualquer sync.
        write(dirA, "user/save/a.sav", "versao do Android")
        Thread.sleep(10) // garante mtime distinto
        write(dirB, "user/save/a.sav", "versao do PC")

        syncEmulator(a, "eden", storageA, baseA)
        val conflicted = syncEmulator(b, "eden", storageB, baseB)
        assertEquals(1, conflicted.conflicts, "edição dos dois lados é conflito")

        // A invariante que mais importa: nenhuma versão some.
        val contents = File(dirB, "user/save").listFiles()!!.map { it.readText() }
        assertTrue("versao do Android" in contents, "versão do Android sumiu: $contents")
        assertTrue("versao do PC" in contents, "versão do PC sumiu: $contents")
    }

    @Test
    fun `change serializes to the shape the rust server expects`() {
        // Não precisa de server: trava a forma do JSON, que é o contrato.
        val put = json.encodeToString(Change.put("a.sav", 10, 100, "abc"))
        assertTrue(""""op":"put"""" in put, put)
        assertTrue(""""path":"a.sav"""" in put, put)
        assertTrue(""""size":10""" in put, put)
        assertTrue(""""hash":"abc"""" in put, put)

        val del = json.encodeToString(Change.delete("b.sav"))
        assertTrue(""""op":"delete"""" in del, del)
        assertTrue("hash" !in del, "delete não deve carregar campos de put: $del")
    }

    @Test
    fun `local changes ignore rewrites with identical content`() {
        val baseline = mapOf("a.sav" to FileMeta(5, 100, "h1"))
        val current = mapOf("a.sav" to FileMeta(5, 999, "h1"))
        assertContentEquals(emptyList(), localChanges(baseline, current))
    }

    @Test
    fun `local changes detect edits and deletions`() {
        val baseline = mapOf(
            "a.sav" to FileMeta(5, 100, "h1"),
            "sumiu.sav" to FileMeta(5, 100, "h2"),
        )
        val current = mapOf("a.sav" to FileMeta(6, 200, "novo"))

        val changes = localChanges(baseline, current)
        assertEquals(2, changes.size)
        assertTrue(changes.any { it.path == "a.sav" && it.op == "put" })
        assertTrue(changes.any { it.path == "sumiu.sav" && it.op == "delete" })
    }

    @Test
    fun `eden syncs only the save subtrees`() {
        // Mesma whitelist do lado Rust: o NAND do eden tem gigabytes de
        // conteúdo de sistema que não é save.
        assertEquals(listOf("system/save/8000000000000010", "user/save"), syncSubtrees("eden"))
        assertEquals(listOf(""), syncSubtrees("pcsx2"))
    }
}
