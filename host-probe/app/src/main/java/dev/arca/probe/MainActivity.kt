package dev.arca.probe

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Typeface
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Log
import android.view.Gravity
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File
import java.io.IOException
import kotlin.concurrent.thread

/**
 * Home de Arca (r10) — el "lanzador" de sub-apps nativas.
 *
 * La vieja pantalla del probe F0 (botón hello + log) cumplió su ciclo: el
 * gate fue GO hace revisiones y el log vivía en logcat. El home ahora es
 * mínimo y útil:
 *
 *  1. **Ejecutar demo incorporada** — lanza [DemoActivity] con el
 *     `devapp-demo` empaquetado en el APK (asset).
 *  2. **Abrir binario desde el almacenamiento…** — picker SAF
 *     (ACTION_OPEN_DOCUMENT): copia el archivo elegido a
 *     `filesDir/exec/` con chmod 7→ (la MISMA grieta de targetSdk 28
 *     que ya usa el demo: dominio SELinux untrusted_app_27 permite
 *     execve en /data/data) y lo lanza en el visor.
 *
 *     Sirve para CUALQUIER binario compilado para el contrato de sub-app:
 *     ELF aarch64 estático (musl), frames JSON por stdout, touch JSON por
 *     stdin, env ARCA_FB/ARCA_FB_W/ARCA_FB_H — o sea, cualquier devapp de
 *     este repo (`./arca.sh build` produce exactamente eso).
 *  3. **Lista de instaladas** — lo ya copiado en filesDir/exec: un toque
 *     lo ejecuta, un toque largo lo borra. Se refresca en cada onResume.
 *
 * Sin permisos: SAF solo lee el URI que el usuario eligió.
 */
class MainActivity : Activity() {

    private lateinit var listContainer: LinearLayout

    private companion object {
        private const val TAG = "ArcaProbe"
        private const val REQ_ABRIR = 42
        private const val DIR_EXEC = "exec"
        private const val MAX_NOMBRE = 64
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val pad = dp(16)
        val title = TextView(this).apply {
            text = getString(R.string.home_title)
            textSize = 28f
            setTextColor(0xFFE8EAF0.toInt())
            setPadding(pad, pad, pad, 0)
        }
        val sub = TextView(this).apply {
            text = getString(R.string.home_sub)
            textSize = 12f
            setTextColor(0xFF9AA3B5.toInt())
            setPadding(pad, dp(4), pad, pad)
        }
        val btnDemo = Button(this).apply {
            text = getString(R.string.btn_run_demo)
            setOnClickListener {
                startActivity(Intent(this@MainActivity, DemoActivity::class.java))
            }
        }
        val btnAbrir = Button(this).apply {
            text = getString(R.string.btn_open)
            setOnClickListener { abrirDesdeAlmacenamiento() }
        }
        val header = TextView(this).apply {
            text = getString(R.string.installed_header)
            textSize = 12f
            typeface = Typeface.MONOSPACE
            setTextColor(0xFF9AA3B5.toInt())
            setPadding(pad, pad, pad, dp(4))
        }
        listContainer = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(pad, 0, pad, pad)
        }

        val scroller = ScrollView(this).apply { addView(listContainer) }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(0xFF101318.toInt())
            addView(title)
            addView(sub)
            addView(btnDemo)
            addView(btnAbrir)
            addView(header)
            addView(
                scroller,
                LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f
                )
            )
        }
        setContentView(root)
    }

    override fun onResume() {
        super.onResume()
        refrescarLista()
    }

    // ───────────────── abrir binario desde el almacenamiento ─────────────────

    /** Picker SAF: cualquier archivo (los devapp son ELF sin extensión). */
    private fun abrirDesdeAlmacenamiento() {
        val intent = Intent(Intent.ACTION_OPEN_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "*/*"
        }
        @Suppress("DEPRECATION") // sin AndroidX: vía clásica
        startActivityForResult(intent, REQ_ABRIR)
    }

    @Deprecated("Deprecated in Java")
    @Suppress("DEPRECATION")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_ABRIR) return
        val uri = data?.data ?: return
        if (resultCode != RESULT_OK) return

        // Copiar fuera del hilo de UI (el archivo puede ser de MBs).
        thread(name = "arca-instala") { instalarDesdeUri(uri) }
    }

    /** Copia el URI elegido a filesDir/exec/<nombre> + chmod y lo lanza. */
    private fun instalarDesdeUri(uri: Uri) {
        try {
            val nombre = sanitizar(nombreDe(uri))
            val dir = File(filesDir, DIR_EXEC)
            if (!dir.exists() && !dir.mkdirs()) {
                throw IOException("no pude crear ${dir.path}")
            }
            val bin = File(dir, nombre)
            contentResolver.openInputStream(uri)?.use { input ->
                bin.outputStream().use { output -> input.copyTo(output, 64 * 1024) }
            } ?: throw IOException("contentResolver no pudo abrir $uri")

            // rwx: sin el bit +x, execve() falla con EACCES aunque SELinux
            // lo permita (misma razón que el installBinary del probe F0).
            val ok = bin.setReadable(true, false) &&
                bin.setWritable(true, false) &&
                bin.setExecutable(true, false)
            if (!ok) throw IOException("chmod falló sobre ${bin.path}")

            Log.i(TAG, "instalada: ${bin.path} (${bin.length()} B)")
            toast(getString(R.string.toast_installed, nombre))
            lanzar(bin)
        } catch (t: Throwable) {
            Log.e(TAG, "instalar FALLÓ", t)
            toast(getString(R.string.toast_copy_fail, t.message ?: "?"))
        }
    }

    /** Nombre visible del documento (SAF); fallback razonable si no hay. */
    private fun nombreDe(uri: Uri): String {
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)
            ?.use { cursor ->
                val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (idx >= 0 && cursor.moveToFirst()) {
                    cursor.getString(idx)?.let { return it }
                }
            }
        return "app-${System.currentTimeMillis()}"
    }

    /** Deja solo [A-Za-z0-9._-] y acorta: será un nombre de archivo. */
    private fun sanitizar(nombre: String): String {
        val limpio = nombre.replace(Regex("[^A-Za-z0-9._-]"), "_")
            .trimStart('.')
            .ifEmpty { "app" }
        return limpio.take(MAX_NOMBRE)
    }

    private fun lanzar(bin: File) {
        val intent = Intent(this, DemoActivity::class.java).apply {
            putExtra("bin", bin.absolutePath)
        }
        runOnUiThread { startActivity(intent) }
    }

    // ───────────────── lista de instaladas ─────────────────

    private fun refrescarLista() {
        listContainer.removeAllViews()
        val dir = File(filesDir, DIR_EXEC)
        val bins = dir.listFiles { f: File -> f.isFile }
            ?.sortedBy { it.name.lowercase() }
            ?: emptyList()

        if (bins.isEmpty()) {
            listContainer.addView(TextView(this).apply {
                text = getString(R.string.installed_empty)
                textSize = 12f
                typeface = Typeface.MONOSPACE
                setTextColor(0xFF6B7385.toInt())
            })
            return
        }

        for (bin in bins) {
            val row = Button(this).apply {
                text = getString(R.string.installed_row, bin.name, kb(bin.length()))
                isAllCaps = false
                gravity = Gravity.START or Gravity.CENTER_VERTICAL
                setOnClickListener { lanzar(bin) }
                setOnLongClickListener { confirmarBorrado(bin); true }
            }
            listContainer.addView(row)
        }
    }

    private fun confirmarBorrado(bin: File) {
        AlertDialog.Builder(this)
            .setMessage(getString(R.string.confirm_delete, bin.name))
            .setPositiveButton(android.R.string.yes) { _, _ ->
                val ok = bin.delete()
                toast(getString(if (ok) R.string.toast_deleted else R.string.toast_delete_fail, bin.name))
                refrescarLista()
            }
            .setNegativeButton(android.R.string.no, null)
            .show()
    }

    private fun kb(bytes: Long): String =
        if (bytes >= 1024 * 1024) "%.1f MB".format(bytes / 1024.0 / 1024.0)
        else "%d KB".format(bytes / 1024)

    private fun toast(msg: String) {
        runOnUiThread {
            Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
            Log.i(TAG, msg)
        }
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()
}
