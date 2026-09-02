package dev.arca.probe

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.util.Log
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.widget.FrameLayout
import android.widget.GridLayout
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import java.io.File
import java.io.IOException
import java.io.RandomAccessFile
import kotlin.concurrent.thread

/**
 * Lanzador de sub-apps nativas (r11).
 *
 * Pantalla principal rediseñada tras el feedback de r10:
 *  · GRID de apps instaladas (icono + nombre) — tocar ejecuta, mantener
 *    pulsado desinstala. Las apps instaladas viven en `filesDir/exec`.
 *  · Un único botón flotante circular "+" para abrir un binario desde el
 *    almacenamiento (SAF ACTION_OPEN_DOCUMENT): se copia a filesDir/exec
 *    con chmod +x y se lanza. Sin botones de texto, sin datos técnicos.
 *  · La demo incorporada del APK se RETIRÓ (r11): el usuario ya carga sus
 *    propios binarios con el +.
 *
 * ICONO Y NOMBRE desde la compilación: los binarios pueden llevar un
 * footer ARCAAPP1 (scripts/empaqueta_app.py lo agrega al compilar con
 * `--name` e `--icono`): [nombre u16-len][PNG u32-len][b"ARCAAPP1"] al
 * final del ELF (el loader ignora los bytes tras el último segmento, así
 * que sigue siendo ejecutable tal cual). Sin footer: avatar con la inicial.
 *
 * Sin permisos: SAF solo lee el URI que el usuario eligió.
 */
class MainActivity : Activity() {

    private lateinit var grid: GridLayout

    /** App instalada lista para pintar en el grid. */
    private data class AppInstalada(val bin: File, val nombre: String, val icono: Bitmap)

    private companion object {
        private const val TAG = "ArcaProbe"
        private const val REQ_ABRIR = 42
        private const val DIR_EXEC = "exec"
        private const val MAX_NOMBRE = 64
        // espejo de scripts/empaqueta_app.py (footer ARCAAPP1)
        private val MAGIC = "ARCAAPP1".toByteArray(Charsets.US_ASCII)
        private const val MAX_ICONO_B = 256 * 1024
        private const val MAX_NOMBRE_META = 96
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val pad = dp(16)

        val titulo = TextView(this).apply {
            text = getString(R.string.apps_title)
            textSize = 20f
            setTextColor(0xFFE8EAF0.toInt())
            setPadding(pad, pad, pad, dp(6))
        }

        grid = GridLayout(this).apply {
            columnCount = if (resources.displayMetrics.widthPixels >= dp(600)) 4 else 3
            setPadding(pad, dp(4), pad, dp(96))   // fondo: no quedar bajo el FAB
        }
        val scroller = ScrollView(this).apply { addView(grid) }

        val contenido = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(titulo)
            addView(
                scroller,
                LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f
                )
            )
        }

        // Botón flotante circular "+": el ÚNICO modo de añadir apps.
        val fab = TextView(this).apply {
            text = "+"
            textSize = 30f
            setTextColor(Color.WHITE)
            gravity = Gravity.CENTER
            background = GradientDrawable().apply {
                shape = GradientDrawable.OVAL
                setColor(0xFF14B8A6.toInt())
            }
            elevation = dp(6).toFloat()
            contentDescription = getString(R.string.fab_abrir)
            setOnClickListener { abrirDesdeAlmacenamiento() }
        }

        val root = FrameLayout(this).apply {
            setBackgroundColor(0xFF101318.toInt())
            addView(
                contenido,
                FrameLayout.LayoutParams(
                    FrameLayout.LayoutParams.MATCH_PARENT,
                    FrameLayout.LayoutParams.MATCH_PARENT
                )
            )
            addView(
                fab,
                FrameLayout.LayoutParams(dp(56), dp(56), Gravity.BOTTOM or Gravity.END).apply {
                    rightMargin = pad
                    bottomMargin = pad
                }
            )
        }
        setContentView(root)
    }

    override fun onResume() {
        super.onResume()
        refrescarGrid()
    }

    // ───────────────── grid de instaladas ─────────────────

    /** Lee filesDir/exec en un hilo propio (decodifica iconos) y pinta. */
    private fun refrescarGrid() {
        thread(name = "arca-lista") {
            val dir = File(filesDir, DIR_EXEC)
            val apps = dir.listFiles { f: File -> f.isFile }
                ?.sortedBy { it.name.lowercase() }
                ?.map { leerInstalada(it) }
                ?: emptyList()
            runOnUiThread { pintarGrid(apps) }
        }
    }

    /** Resuelve nombre+icono de un binario (footer o fallback). */
    private fun leerInstalada(bin: File): AppInstalada {
        val meta = leerFooter(bin)
        val nombre = meta?.first?.takeIf { it.isNotBlank() }
            ?: bin.name.substringBeforeLast('.')
        return AppInstalada(bin, nombre, meta?.second ?: avatar(nombre))
    }

    /**
     * Footer ARCAAPP1 (espejo de scripts/empaqueta_app.py):
     * [nombre utf-8][u16 len][icono PNG][u32 len][magic 8] al final.
     * Devuelve (nombre, icono) o null si no hay footer válido.
     */
    private fun leerFooter(bin: File): Pair<String, Bitmap?>? {
        try {
            RandomAccessFile(bin, "r").use { raf ->
                val len = raf.length()
                if (len < 14L) return null
                val magic = ByteArray(8)
                raf.seek(len - 8)
                raf.readFully(magic)
                if (!magic.contentEquals(MAGIC)) return null

                raf.seek(len - 12)
                val b4 = ByteArray(4)
                raf.readFully(b4)
                val iconoLen = u32le(b4)
                if (iconoLen < 0 || iconoLen > MAX_ICONO_B) return null
                val iconoStart = len - 12 - iconoLen
                if (iconoStart < 2L) return null

                raf.seek(iconoStart - 2)
                val b2 = ByteArray(2)
                raf.readFully(b2)
                val nombreLen = (b2[0].toInt() and 0xFF) or
                    ((b2[1].toInt() and 0xFF) shl 8)
                if (nombreLen <= 0 || nombreLen > MAX_NOMBRE_META) return null

                val nombreStart = iconoStart - 2 - nombreLen
                if (nombreStart < 0) return null

                raf.seek(nombreStart)
                val nombreB = ByteArray(nombreLen)
                raf.readFully(nombreB)
                val nombre = String(nombreB, Charsets.UTF_8).trim()

                var icono: Bitmap? = null
                if (iconoLen > 0) {
                    val iconoB = ByteArray(iconoLen.toInt())
                    raf.seek(iconoStart)
                    raf.readFully(iconoB)
                    icono = decodificarIcono(iconoB)
                }
                return nombre to icono
            }
        } catch (_: Exception) {
            return null   // footer corrupto: fallback del caller
        }
    }

    private fun u32le(b: ByteArray): Long =
        (b[0].toLong() and 0xFF) or
            ((b[1].toLong() and 0xFF) shl 8) or
            ((b[2].toLong() and 0xFF) shl 16) or
            ((b[3].toLong() and 0xFF) shl 24)

    /** PNG→Bitmap con muestreo acotado (un icono malvado no tumba al lanzador). */
    private fun decodificarIcono(bytes: ByteArray): Bitmap? {
        if (bytes.size < 8 || bytes[0] != 0x89.toByte() || bytes[1] != 'P'.code.toByte()) {
            return null
        }
        val sondear = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, sondear)
        if (sondear.outWidth <= 0 || sondear.outHeight <= 0) return null
        var sample = 1
        while (sondear.outWidth / (sample * 2) >= 128 &&
            sondear.outHeight / (sample * 2) >= 128
        ) {
            sample *= 2
        }
        val opts = BitmapFactory.Options().apply { inSampleSize = sample }
        return try {
            BitmapFactory.decodeByteArray(bytes, 0, bytes.size, opts)
        } catch (_: Exception) {
            null
        }
    }

    /** Avatar de respaldo: círculo teal con la inicial del nombre. */
    private fun avatar(nombre: String): Bitmap {
        val sz = 96
        val bmp = Bitmap.createBitmap(sz, sz, Bitmap.Config.ARGB_8888)
        val cv = Canvas(bmp)
        val p = Paint(Paint.ANTI_ALIAS_FLAG).apply {
            color = 0xFF0F766E.toInt()
            cv.drawCircle(sz / 2f, sz / 2f, sz / 2f, this)
            color = Color.WHITE
            textSize = 52f
            textAlign = Paint.Align.CENTER
            isFakeBoldText = true
        }
        val letra = nombre.trim().firstOrNull { it.isLetterOrDigit() }
            ?.uppercaseChar()?.toString() ?: "?"
        val y = sz / 2f - (p.ascent() + p.descent()) / 2f
        cv.drawText(letra, sz / 2f, y, p)
        return bmp
    }

    /** Pinta el grid (hilo UI): celdas icono+nombre o el estado vacío. */
    private fun pintarGrid(apps: List<AppInstalada>) {
        grid.removeAllViews()
        if (apps.isEmpty()) {
            val vacio = TextView(this).apply {
                text = getString(R.string.apps_empty)
                textSize = 14f
                gravity = Gravity.CENTER
                setTextColor(0xFF6B7385.toInt())
                setPadding(0, dp(48), 0, dp(48))
            }
            // r13: el peso va en la COLUMNA (con span total para el
            // estado vacío). En r11 el peso estaba en la FILA: columnas
            // sin peso + width 0 → columnas de 0 px → el grid quedaba
            // INVISIBLE ("no genera ninguna lista") aunque el escaneo de
            // filesDir/exec funcionaba y las celdas existían.
            val lp = GridLayout.LayoutParams(
                GridLayout.spec(GridLayout.UNDEFINED),
                GridLayout.spec(GridLayout.UNDEFINED, grid.columnCount, 1f)
            )
            lp.width = 0
            vacio.layoutParams = lp
            grid.addView(vacio)
            return
        }
        for (app in apps) {
            grid.addView(celda(app))
        }
    }

    /** Celda del grid: icono 56dp + nombre (2 líneas máx). */
    private fun celda(app: AppInstalada): View {
        val icono = ImageView(this).apply {
            setImageBitmap(app.icono)
            scaleType = ImageView.ScaleType.FIT_CENTER
            adjustViewBounds = false
        }
        val nombre = TextView(this).apply {
            text = app.nombre
            textSize = 11f
            gravity = Gravity.CENTER
            maxLines = 2
            setTextColor(0xFFC7CDDA.toInt())
            setPadding(0, dp(6), 0, 0)
        }
        val celda = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(6), dp(12), dp(6), dp(12))
            isClickable = true
            isFocusable = true
            // feedback táctil del tema (sin AndroidX)
            val tv = TypedValue()
            theme.resolveAttribute(android.R.attr.selectableItemBackground, tv, true)
            if (tv.resourceId != 0) setBackgroundResource(tv.resourceId)
            setOnClickListener { lanzar(app.bin) }
            setOnLongClickListener { confirmarBorrado(app.bin, app.nombre); true }
            addView(icono, LinearLayout.LayoutParams(dp(56), dp(56)))
            addView(
                nombre,
                LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT,
                    LinearLayout.LayoutParams.WRAP_CONTENT
                )
            )
        }
        // r13: peso en la COLUMNA (1 columna, weight 1) → cada celda toma
        // 1/columnCount del ancho. El bug de r11: el peso estaba en el
        // ROW spec (inútil dentro de un ScrollView) y las columnas
        // quedaban sin peso con width=0 → columnas de 0 px → celdas
        // invisibles con el escaneo funcionando.
        val lp = GridLayout.LayoutParams(
            GridLayout.spec(GridLayout.UNDEFINED),
            GridLayout.spec(GridLayout.UNDEFINED, 1, 1f)
        )
        lp.width = 0
        celda.layoutParams = lp
        return celda
    }

    private fun confirmarBorrado(bin: File, nombre: String) {
        AlertDialog.Builder(this)
            .setMessage(getString(R.string.confirm_delete, nombre))
            .setPositiveButton(android.R.string.yes) { _, _ ->
                val ok = bin.delete()
                toast(
                    getString(
                        if (ok) R.string.toast_deleted else R.string.toast_delete_fail,
                        nombre
                    )
                )
                refrescarGrid()
            }
            .setNegativeButton(android.R.string.no, null)
            .show()
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

            // rwx: sin el bit +x, execve() falla con EACCES (misma razón
            // que el installBinary del probe F0).
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

    private fun toast(msg: String) {
        runOnUiThread {
            Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
            Log.i(TAG, msg)
        }
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()
}
