package net.savesync.protocol

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * Tipos do wire do protocolo de sync.
 *
 * Espelham `crates/core/src/protocol.rs` campo a campo. O que garante que
 * continuem espelhando não é disciplina: é o teste de integração, que roda
 * contra o binário do server de verdade. Se um nome divergir, ele quebra.
 *
 * Está em Kotlin/JVM puro, sem nada de Android, porque é a parte que dá pra
 * testar sem aparelho — e é a parte onde estar errado sai caro.
 */

val json = Json {
    ignoreUnknownKeys = true
    encodeDefaults = true
    // Sem isto, um `delete` sairia com `"size":null,"mtime":null,"hash":null`.
    // O Rust tolera (ignora campo desconhecido em enum com tag interna), mas
    // depender dessa tolerância é frágil: o JSON deve ser o mesmo que o
    // `#[serde(tag = "op")]` produz, não um superconjunto que por acaso passa.
    explicitNulls = false
}

/**
 * Mudança local reportada ao server. A forma no JSON é achatada com uma tag
 * `op`, igual ao `#[serde(tag = "op")]` do lado Rust:
 *
 *     {"path": "...", "op": "put", "size": 10, "mtime": 1, "hash": "..."}
 *     {"path": "...", "op": "delete"}
 *
 * Kotlin não tem o equivalente direto de enum com campos achatados no JSON,
 * então é uma data class com os campos opcionais e um `op` explícito. Menos
 * elegante que o lado Rust, mas produz exatamente o mesmo JSON — que é o
 * que importa.
 */
@Serializable
data class Change(
    val path: String,
    val op: String,
    val size: Long? = null,
    val mtime: Long? = null,
    val hash: String? = null,
) {
    companion object {
        fun put(path: String, size: Long, mtime: Long, hash: String) =
            Change(path = path, op = "put", size = size, mtime = mtime, hash = hash)

        fun delete(path: String) = Change(path = path, op = "delete")
    }
}

@Serializable
data class PlanRequest(
    @SerialName("last_rev") val lastRev: Long,
    val changes: List<Change>,
)

@Serializable
data class UploadItem(val path: String)

@Serializable
data class DownloadItem(
    val path: String,
    val rev: Long,
    val size: Long,
    val mtime: Long,
    val hash: String,
)

@Serializable
data class DeleteItem(val path: String)

@Serializable
data class ConflictItem(
    val path: String,
    /** `"client"` ou `"server"` — quem venceu. */
    val winner: String,
    @SerialName("loser_path") val loserPath: String,
)

@Serializable
data class PlanResponse(
    val session: String,
    @SerialName("head_rev") val headRev: Long,
    val upload: List<UploadItem> = emptyList(),
    val download: List<DownloadItem> = emptyList(),
    @SerialName("delete_local") val deleteLocal: List<DeleteItem> = emptyList(),
    val conflicts: List<ConflictItem> = emptyList(),
)

@Serializable
data class CommitResponse(@SerialName("new_rev") val newRev: Long)

@Serializable
data class PairResponse(
    @SerialName("device_id") val deviceId: String,
    @SerialName("device_token") val deviceToken: String,
)

/** Erro do server, com o código estável que a UI traduz. */
class ApiException(val code: String, val status: Int, val detail: String? = null) :
    RuntimeException(detail?.let { "$code: $it" } ?: code)

/** Metadados de um arquivo. `hash` é SHA-256 em hex minúsculo. */
data class FileMeta(val size: Long, val mtime: Long, val hash: String)

/** Retrato da árvore no fim do último sync aceito. */
data class Baseline(
    val lastRev: Long = 0,
    val files: Map<String, FileMeta> = emptyMap(),
)

data class SyncReport(
    val uploaded: Int,
    val downloaded: Int,
    val deleted: Int,
    val conflicts: Int,
    val newRev: Long,
    val baseline: Baseline,
)
