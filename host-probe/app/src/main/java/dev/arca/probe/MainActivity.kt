package dev.arca.probe

import android.app.Activity
import android.graphics.Typeface
import android.os.Bundle
import android.util.Log
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import java.io.BufferedReader
import java.io.File
import java.io.IOException
import java.io.InputStreamReader
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/**
 * Probe F0 de Arca — gate GO/NO-GO del proyecto (tarea T02).
 *
 * Flujo al pulsar el botón:
 *  1. Copia el asset `devapp-hello` (ELF aarch64 estático-PIE compilado con
 *     cargo-ndk) a `filesDir/devapp-hello` y le aplica chmod 700.
 *  2. Lo lanza como proceso hijo (fork+exec vía [ProcessBuilder]) — LA
 *     prueba del gate: si el dominio SELinux del APK lo permite, el hijo vive.
 *  3. Un hilo lee su stdout línea a línea y lo manda a Logcat (tag
 *     `ArcaProbe`) y a pantalla (últimas líneas).
 *  4. Watchdog de 30 s: `destroy()` manda SIGTERM (ejerce el handler del
 *     binario: debe responder `{"event":"sigterm",...}` y salir 0 en ≤100 ms);
 *     si no muere, `destroyForcibly()`.
 *
 * Interpretación del resultado → rellenar `host-probe/decision.md`:
 * heartbeats visibles = exec OK = GO para el backend nativo; error
 * "Permission denied" en el launch = NO-GO (pivot WASM, docs/12 F5).
 */
class MainActivity : Activity() {

    private lateinit var statusView: TextView
    private lateinit var logView: TextView
    private lateinit var launchButton: Button

    /** Buffer en pantalla; protegido con [logLock] (lector + hilo UI). */
    private val logLock = Any()
    private val logLines = ArrayDeque<String>()

    private companion object {
        private const val TAG = "ArcaProbe"
        private const val ASSET_NAME = "devapp-hello"
        private const val MAX_LOG_LINES = 60
        private const val WATCHDOG_S = 30L
        private const val GRACE_AFTER_SIGTERM_S = 3L
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val pad = dp(16)
        statusView = TextView(this).apply {
            text = getString(R.string.status_idle)
            setPadding(pad, pad, pad, 0)
        }
        launchButton = Button(this).apply {
            text = getString(R.string.btn_launch)
            setOnClickListener { onLaunchClicked() }
        }
        logView = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            textSize = 12f
            setTextIsSelectable(true)
            setPadding(pad, pad / 2, pad, pad)
        }
        val scroller = ScrollView(this).apply { addView(logView) }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(statusView)
            addView(launchButton)
            addView(
                scroller,
                LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f)
            )
        }
        setContentView(root)
    }

    /** Lanza el probe fuera del hilo de UI (nunca bloquear el main thread). */
    private fun onLaunchClicked() {
        launchButton.isEnabled = false
        synchronized(logLock) { logLines.clear() }
        renderLog()
        postStatus(getString(R.string.status_starting))
        thread(name = "arca-probe") { runProbe() }
    }

    /** Ejecuta el flujo completo del probe (en hilo propio). */
    private fun runProbe() {
        try {
            val bin = installBinary()
            postStatus("Ejecutando: ${bin.absolutePath}")

            // fork+exec del binario extraído — EL experimento del gate.
            val process = ProcessBuilder(bin.absolutePath)
                .redirectErrorStream(true) // stderr también al logcat del probe
                .start()

            // Lector de stdout: línea a línea → Logcat + pantalla.
            thread(name = "arca-probe-stdout") { pumpStdout(process) }

            // Watchdog de seguridad: 30 s y SIGTERM (destroy()).
            val finished = process.waitFor(WATCHDOG_S, TimeUnit.SECONDS)
            if (!finished) {
                postStatus("Watchdog ${WATCHDOG_S}s → destroy() [SIGTERM]")
                Log.i(TAG, "watchdog: destroy() → SIGTERM al hijo")
                process.destroy()
                if (!process.waitFor(GRACE_AFTER_SIGTERM_S, TimeUnit.SECONDS)) {
                    postStatus("SIGTERM ignorado → destroyForcibly()")
                    process.destroyForcibly()
                    process.waitFor()
                }
            }

            val code = process.exitValue()
            Log.i(TAG, "exit code = $code")
            postStatus(getString(R.string.status_exited, code))
        } catch (t: Throwable) {
            // El caso NO-GO típico llega aquí: IOException con
            // "error=13, Permission denied" (EACCES de execve bajo W^X).
            Log.e(TAG, "probe FAILED", t)
            postStatus("FAIL: ${t.message}")
        } finally {
            runOnUiThread { launchButton.isEnabled = true }
        }
    }

    /** Lee stdout del hijo hasta EOF; cada línea → Logcat + UI. */
    private fun pumpStdout(process: Process) {
        try {
            BufferedReader(InputStreamReader(process.inputStream, Charsets.UTF_8)).use { reader ->
                while (true) {
                    val line = reader.readLine() ?: break
                    Log.i(TAG, line)
                    appendLog(line)
                }
            }
        } catch (e: IOException) {
            // Normal cuando el proceso muere con el pipe a medias; el wait()
            // del padre reporta el exit code real.
            Log.w(TAG, "stdout reader: ${e.message}")
        }
    }

    /**
     * Extrae el binario del APK (assets/) a filesDir/ con chmod 700.
     *
     * La extracción a `/data/data/<pkg>/files` + el exec posterior ES la
     * grieta de targetSdk 28 que este probe valida (blueprint docs/01 §2).
     */
    private fun installBinary(): File {
        val bin = File(filesDir, ASSET_NAME)
        if (bin.exists() && !bin.delete()) {
            throw IOException("no pude borrar ${bin.path} (¿hay un probe vivo?)")
        }
        try {
            assets.open(ASSET_NAME).use { input ->
                bin.outputStream().use { output -> input.copyTo(output, 64 * 1024) }
            }
        } catch (e: IOException) {
            throw IOException(
                "asset '$ASSET_NAME' no encontrado o ilegible — coloca el binario " +
                    "compilado con cargo-ndk en app/src/main/assets/ (ver su README.md)",
                e
            )
        }
        // rwx------: sin el bit +x, execve() falla con EACCES aunque SELinux
        // lo permita. (En Termux esto es el clásico "chmod +x".)
        val permsOk = bin.setReadable(true, false) &&
            bin.setWritable(true, false) &&
            bin.setExecutable(true, false)
        if (!permsOk) {
            throw IOException("chmod 700 falló sobre ${bin.path}")
        }
        Log.i(TAG, "instalado: ${bin.path} (${bin.length()} B)")
        return bin
    }

    /** Añade una línea al buffer en pantalla (cap 60) y refresca el TextView. */
    private fun appendLog(line: String) {
        val snapshot = synchronized(logLock) {
            logLines.addLast(line)
            while (logLines.size > MAX_LOG_LINES) logLines.removeFirst()
            logLines.joinToString("\n")
        }
        runOnUiThread { logView.text = snapshot }
    }

    private fun renderLog() {
        val snapshot = synchronized(logLock) { logLines.joinToString("\n") }
        logView.text = snapshot
    }

    private fun postStatus(msg: String) {
        runOnUiThread {
            statusView.text = msg
            Log.i(TAG, msg)
        }
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()
}
