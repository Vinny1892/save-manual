package net.savesync.protocol

import java.io.File

/**
 * [SaveStorage] sobre filesystem comum.
 *
 * Serve nos testes e num aparelho onde o caminho é alcançável direto — root
 * ou ROM custom, via `/data/media/0/Android/data/...`. Num Android de
 * fábrica esta implementação **não funciona** pro eden: o caminho é
 * bloqueado pelo sistema, e a saída passa por SAF ou Shizuku. Ver o README.
 */
class LocalFileStorage(private val root: File) : SaveStorage {

    /**
     * `known` permite reusar o hash quando `size` e `mtime` batem, em vez de
     * reler o arquivo. Numa árvore parada, um ciclo não lê conteúdo nenhum.
     */
    var known: Map<String, FileMeta> = emptyMap()

    override fun list(subtrees: List<String>): Map<String, FileMeta> {
        val out = LinkedHashMap<String, FileMeta>()
        for (sub in subtrees) {
            val start = if (sub.isEmpty()) root else File(root, sub)
            if (!start.isDirectory) continue
            walk(start, out)
        }
        return out
    }

    private fun walk(dir: File, out: MutableMap<String, FileMeta>) {
        val entries = dir.listFiles() ?: return
        for (entry in entries) {
            when {
                entry.isDirectory -> walk(entry, out)
                entry.isFile -> {
                    val rel = relative(entry) ?: continue
                    val size = entry.length()
                    val mtime = entry.lastModified()
                    val prev = known[rel]
                    val hash =
                        if (prev != null && prev.size == size && prev.mtime == mtime) prev.hash
                        else sha256(entry.readBytes())
                    out[rel] = FileMeta(size, mtime, hash)
                }
            }
        }
    }

    /** Caminho relativo com separador POSIX, que é o que o protocolo usa. */
    private fun relative(file: File): String? {
        val rel = file.relativeToOrNull(root) ?: return null
        return rel.path.replace(File.separatorChar, '/')
    }

    override fun read(path: String) = File(root, path).readBytes()

    override fun write(path: String, bytes: ByteArray, mtime: Long) {
        val dest = File(root, path)
        dest.parentFile?.mkdirs()
        dest.writeBytes(bytes)
        dest.setLastModified(mtime)
    }

    override fun delete(path: String) {
        val target = File(root, path)
        if (!target.exists()) return
        target.delete()
        // Sobe apagando diretório que ficou vazio, parando na raiz — save
        // deletado não pode deixar esqueleto de pasta, que a UI mostraria
        // como jogo fantasma.
        var dir = target.parentFile
        while (dir != null && dir != root && dir.startsWith(root)) {
            if (dir.list()?.isNotEmpty() != false) break
            if (!dir.delete()) break
            dir = dir.parentFile
        }
    }

    override fun rename(from: String, to: String) {
        val src = File(root, from)
        if (!src.exists()) return
        val dst = File(root, to)
        dst.parentFile?.mkdirs()
        src.renameTo(dst)
    }
}
