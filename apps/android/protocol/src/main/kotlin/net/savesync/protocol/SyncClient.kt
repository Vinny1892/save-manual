package net.savesync.protocol

import java.security.MessageDigest
import kotlinx.serialization.encodeToString
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response

/**
 * Onde os saves ficam.
 *
 * É uma abstração e não `java.io.File` de propósito: no Android o acesso
 * pode vir por SAF (`DocumentFile`), por Shizuku, ou por caminho direto num
 * aparelho com root — e a lógica de sync não deve saber a diferença. No
 * desktop e nos testes, [LocalFileStorage] implementa por filesystem comum.
 *
 * A restrição que motiva isso está no README deste diretório: desde o
 * Android 11, `Android/data/<pacote>` não é alcançável por outro app, e qual
 * das saídas usar depende do aparelho.
 */
interface SaveStorage {
    /** Todos os arquivos sob as subárvores sincronizadas. */
    fun list(subtrees: List<String>): Map<String, FileMeta>
    fun read(path: String): ByteArray
    fun write(path: String, bytes: ByteArray, mtime: Long)
    fun delete(path: String)
    fun rename(from: String, to: String)
}

fun sha256(bytes: ByteArray): String =
    MessageDigest.getInstance("SHA-256").digest(bytes)
        .joinToString("") { "%02x".format(it) }

/**
 * O que mudou desde o baseline.
 *
 * Conteúdo igual ao do baseline não é mudança, mesmo com mtime novo — o
 * emulador reescreve save o tempo todo, e considerar mtime geraria tráfego
 * a cada partida. Mesma regra do lado Rust.
 */
fun localChanges(baseline: Map<String, FileMeta>, current: Map<String, FileMeta>): List<Change> {
    val changes = mutableListOf<Change>()

    for ((path, meta) in current) {
        val prev = baseline[path]
        if (prev == null || prev.hash != meta.hash) {
            changes += Change.put(path, meta.size, meta.mtime, meta.hash)
        }
    }
    for (path in baseline.keys) {
        if (path !in current) changes += Change.delete(path)
    }
    return changes
}

/** Subárvores que participam do sync, por emulador. Espelha `engine::sync_subtrees`. */
fun syncSubtrees(emulator: String): List<String> = when (emulator) {
    "eden" -> listOf("system/save/8000000000000010", "user/save")
    else -> listOf("")
}

class SyncClient(
    baseUrl: String,
    private val token: String,
    private val http: OkHttpClient = OkHttpClient(),
) {
    private val base = baseUrl.trimEnd('/')

    private fun url(path: String) = "$base/api/v1$path"

    private fun <T> handle(res: Response, parse: (String) -> T): T {
        val body = res.body?.string().orEmpty()
        if (!res.isSuccessful) {
            val code = runCatching {
                json.parseToJsonElement(body).let { el ->
                    (el as? kotlinx.serialization.json.JsonObject)
                        ?.get("error")?.toString()?.trim('"')
                }
            }.getOrNull() ?: "http_${res.code}"
            throw ApiException(code, res.code)
        }
        return parse(body)
    }

    fun plan(emulator: String, lastRev: Long, changes: List<Change>): PlanResponse {
        val payload = json.encodeToString(PlanRequest(lastRev, changes))
        val req = Request.Builder()
            .url(url("/sync/$emulator/plan"))
            .header("authorization", "Bearer $token")
            .post(payload.toRequestBody("application/json".toMediaType()))
            .build()
        return http.newCall(req).execute().use { handle(it) { b -> json.decodeFromString(b) } }
    }

    fun putBlob(
        emulator: String,
        session: String,
        path: String,
        bytes: ByteArray,
        hash: String,
        mtime: Long,
    ) {
        val url = url("/sync/$emulator/blob").toHttpUrl().newBuilder()
            .addQueryParameter("session", session)
            .addQueryParameter("path", path)
            .build()
        val req = Request.Builder()
            .url(url)
            .header("authorization", "Bearer $token")
            .header("x-save-sync-hash", hash)
            .header("x-save-sync-mtime", mtime.toString())
            .put(bytes.toRequestBody("application/octet-stream".toMediaType()))
            .build()
        http.newCall(req).execute().use { handle(it) {} }
    }

    /**
     * Baixa validando por hash, não por `rev`: o `rev` de um arquivo muda
     * entre o plano e o commit quando o server preserva um perdedor de
     * conflito por rename, e pedir por `rev` daria 409 num arquivo íntegro.
     * Ver §5 da spec.
     */
    fun getBlob(emulator: String, path: String, hash: String): ByteArray {
        val url = url("/sync/$emulator/blob").toHttpUrl().newBuilder()
            .addQueryParameter("path", path)
            .addQueryParameter("hash", hash)
            .build()
        val req = Request.Builder()
            .url(url)
            .header("authorization", "Bearer $token")
            .build()
        return http.newCall(req).execute().use { res ->
            if (!res.isSuccessful) throw ApiException("http_${res.code}", res.code)
            res.body!!.bytes()
        }
    }

    fun commit(emulator: String, session: String): CommitResponse {
        val req = Request.Builder()
            .url(url("/sync/$emulator/commit"))
            .header("authorization", "Bearer $token")
            .post(
                """{"session":"$session"}"""
                    .toRequestBody("application/json".toMediaType())
            )
            .build()
        return http.newCall(req).execute().use { handle(it) { b -> json.decodeFromString(b) } }
    }

    companion object {
        /** Troca um código de pareamento por um token. */
        fun pair(
            baseUrl: String,
            code: String,
            deviceName: String,
            platform: String = "android",
            http: OkHttpClient = OkHttpClient(),
        ): PairResponse {
            val payload =
                """{"code":"$code","device_name":"$deviceName","platform":"$platform"}"""
            val req = Request.Builder()
                .url("${baseUrl.trimEnd('/')}/api/v1/pair")
                .post(payload.toRequestBody("application/json".toMediaType()))
                .build()
            return http.newCall(req).execute().use { res ->
                val body = res.body?.string().orEmpty()
                if (!res.isSuccessful) throw ApiException("pair_failed", res.code, body)
                json.decodeFromString(body)
            }
        }
    }
}

/**
 * Um ciclo completo de sync.
 *
 * Mesma ordem do lado Rust, e pelo mesmo motivo: o local só muda **depois**
 * do commit. Se o processo morrer no meio, o baseline antigo continua
 * valendo e o próximo plano recalcula.
 */
fun syncEmulator(
    client: SyncClient,
    emulator: String,
    storage: SaveStorage,
    baseline: Baseline,
): SyncReport {
    val current = storage.list(syncSubtrees(emulator))
    val changes = localChanges(baseline.files, current)

    val planned = client.plan(emulator, baseline.lastRev, changes)

    // O perdedor que este lado segura precisa ser renomeado antes do upload:
    // é com o nome novo que ele sobe.
    val staged = current.toMutableMap()
    for (conflict in planned.conflicts) {
        if (conflict.winner == "server") {
            storage.rename(conflict.path, conflict.loserPath)
            staged.remove(conflict.path)?.let { staged[conflict.loserPath] = it }
        }
    }

    for (item in planned.upload) {
        val meta = staged[item.path]
            ?: throw IllegalStateException("upload pedido de arquivo ausente: ${item.path}")
        client.putBlob(
            emulator, planned.session, item.path,
            storage.read(item.path), meta.hash, meta.mtime,
        )
    }

    val committed = client.commit(emulator, planned.session)

    val next = staged
    for (item in planned.download) {
        val bytes = client.getBlob(emulator, item.path, item.hash)
        storage.write(item.path, bytes, item.mtime)
        next[item.path] = FileMeta(bytes.size.toLong(), item.mtime, sha256(bytes))
    }
    for (item in planned.deleteLocal) {
        storage.delete(item.path)
        next.remove(item.path)
    }

    return SyncReport(
        uploaded = planned.upload.size,
        downloaded = planned.download.size,
        deleted = planned.deleteLocal.size,
        conflicts = planned.conflicts.size,
        newRev = committed.newRev,
        baseline = Baseline(committed.newRev, next),
    )
}
