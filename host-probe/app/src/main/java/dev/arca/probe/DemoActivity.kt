package dev.arca.probe

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color as AColor
import android.graphics.Paint
import android.graphics.Rect
import android.os.Bundle
import android.util.Log
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import java.io.BufferedReader
import java.io.File
import java.io.IOException
import java.io.InputStreamReader
import java.io.OutputStream
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.nio.MappedByteBuffer
import java.util.concurrent.TimeUnit
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * Probe visual F3a — el "display server" de juguete del proyecto.
 *
 * Flujo (idéntico al diagrama graphs/gfx-f3a.mmd):
 *  1. Calcula el tamaño del framebuffer (pantalla escalada, lado mayor
 *     ≤ 720 px) y crea `filesDir/arca-fb.bin` con la geometría EXACTA del
 *     double-buffer seqlock de arca-shm: 2 slots × (16 B de seq + frame).
 *  2. Mapea el archivo RW (FileChannel.map: MAP_SHARED — coherente con
 *     el mmap del hijo) y lo deja abierto toda la sesión.
 *  3. Extrae `devapp-demo` del APK (chmod 700) y lo lanza con
 *     ARCA_FB/ARCA_FB_W/ARCA_FB_H (fork+exec: mismo UID, mismo sandbox).
 *  4. Hilo lector de stdout: `{"event":"frame"}` → lee el frame más nuevo
 *     con el protocolo seqlock (seq impar = válido; copiar y revalidar)
 *     y lo blitea al SurfaceView. `{"event":"stats"}` → línea de estado.
 *  5. Touch → stdin del hijo como JSON (el hijo escala nada: nosotros
 *     convertimos de pantalla a píxeles del framebuffer).
 *
 * Layout del payload de cada slot (espejo de arca-gfx-protocol):
 *  0..4 "AFRM" · 4 version(=1) · 5 formato(1=RGBA) · 7 flags ·
 *  8..10 w(u16 LE) · 10..12 h(u16 LE) · 12..16 frame_seq(u32) ·
 *  16..24 ts_ms(u64) · 24..32 cero · después bitmap RGBA top-down.
 */
class DemoActivity : Activity() {

    private lateinit var statusView: TextView
    private lateinit var surfaceView: SurfaceView
    private var stopButton: Button? = null

    // Estado del proceso hijo (solo toca el hilo "arca-demo").
    private var process: Process? = null
    private var childStdin: OutputStream? = null

    // Framebuffer compartido (host = lector).
    private var fbFile: RandomAccessFile? = null
    private var fbMap: MappedByteBuffer? = null
    private var fbW = 0
    private var fbH = 0
    private var frameBytes = 0   // cabecera + bitmap
    private var slotStride = 0   // 16 (seq+pad) + frameBytes

    // Blit (hilo lector es el único que pinta).
    private var bitmap: Bitmap? = null
    private var pxInts = IntArray(0)      // reasignado UNA vez en setup
    private var blitBuf = ByteArray(0)    // ídem: buffer de copia del frame
    private val paint = Paint().apply { isFilterBitmap = true }
    private val dstRect = Rect()

    @Volatile private var surfaceReady = false
    @Volatile private var running = false
    private var framesBlit = 0L
    private var framesSeen = 0L

    private companion object {
        private const val TAG = "ArcaProbe"
        private const val ASSET = "devapp-demo"
        private const val FB_NAME = "arca-fb.bin"
        private const val SLOT_HDR = 16        // arca-shm: seq u64 + pad 8
        private const val SLOTS = 2            // double-buffer
        private const val HDR = 32             // arca-gfx-protocol
        private const val WATCHDOG_S = 180L
        private const val GRACE_S = 3L
        private const val MAX_LADO = 720       // presupuesto de blit
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        statusView = TextView(this).apply {
            text = getString(R.string.demo_status_idle)
            setPadding(dp(16), dp(12), dp(16), dp(8))
        }
        surfaceView = SurfaceView(this).apply {
            holder.addCallback(object : SurfaceHolder.Callback {
                override fun surfaceCreated(h: SurfaceHolder) {
                    surfaceReady = true
                }

                override fun surfaceChanged(h: SurfaceHolder, format: Int, w: Int, h2: Int) {
                    computeDstRect(w, h2)
                }

                override fun surfaceDestroyed(h: SurfaceHolder) {
                    surfaceReady = false
                }
            })
        }
        stopButton = Button(this).apply {
            text = getString(R.string.demo_btn_stop)
            setOnClickListener { stopChild("botón detener") }
        }
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(statusView)
            addView(
                surfaceView,
                LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f
                )
            )
            addView(
                stopButton,
                LinearLayout.LayoutParams(
                    LinearLayout.LayoutParams.MATCH_PARENT,
                    LinearLayout.LayoutParams.WRAP_CONTENT
                )
            )
        }
        setContentView(root)
    }

    override fun onResume() {
        super.onResume()
        if (!running) {
            running = true
            thread(name = "arca-demo") { runDemo() }
        }
    }

    override fun onDestroy() {
        running = false
        stopChild("activity destruida")
        super.onDestroy()
    }

    // ───────────────────── ciclo del demo ─────────────────────

    private fun runDemo() {
        try {
            setupFramebuffer()
            val bin = installBinary()
            postStatus(getString(R.string.demo_status_running, fbW, fbH))

            val pb = ProcessBuilder(bin.absolutePath)
            pb.redirectErrorStream(true)
            pb.environment()["ARCA_FB"] = fbPath().absolutePath
            pb.environment()["ARCA_FB_W"] = fbW.toString()
            pb.environment()["ARCA_FB_H"] = fbH.toString()
            val p = pb.start()
            process = p
            childStdin = p.outputStream

            thread(name = "arca-demo-stdout") { pumpStdout(p) }

            // Watchdog de seguridad (como el probe F0, pero 180 s).
            val finished = p.waitFor(WATCHDOG_S, TimeUnit.SECONDS)
            if (!finished) {
                postStatus(getString(R.string.demo_status_watchdog))
                Log.i(TAG, "demo watchdog: destroy() → SIGTERM")
                p.destroy()
                if (!p.waitFor(GRACE_S, TimeUnit.SECONDS)) {
                    p.destroyForcibly()
                    p.waitFor()
                }
            }
            val code = p.exitValue()
            Log.i(TAG, "demo exit code = $code (blits: $framesBlit/${framesSeen} frames)")
            postStatus(getString(R.string.demo_status_exited, code, framesBlit))
        } catch (t: Throwable) {
            Log.e(TAG, "demo FAILED", t)
            postStatus("FAIL: ${t.message}")
        } finally {
            running = false
        }
    }

    /** Dimensiona y crea el archivo de región + su mapeo (rol HOST). */
    private fun setupFramebuffer() {
        // Tamaño del framebuffer: pantalla escalada, lado mayor ≤ MAX_LADO,
        // pares (u16 friendly).
        val dm = resources.displayMetrics
        val sw = dm.widthPixels.coerceAtLeast(1)
        val sh = dm.heightPixels.coerceAtLeast(1)
        val scale = minOf(1f, MAX_LADO.toFloat() / maxOf(sw, sh))
        fbW = ((sw * scale).toInt() / 2 * 2).coerceAtLeast(2)
        fbH = ((sh * scale).toInt() / 2 * 2).coerceAtLeast(2)

        frameBytes = HDR + fbW * fbH * 4
        slotStride = SLOT_HDR + frameBytes
        val regionLen = SLOTS * slotStride

        val f = File(filesDir, FB_NAME)
        if (f.exists() && !f.delete()) {
            throw IOException("no pude borrar ${f.path} (¿demo anterior vivo?)")
        }
        val raf = RandomAccessFile(f, "rw")
        raf.setLength(regionLen.toLong())   // cero ⇒ seq par ⇒ sin frame
        val ch = raf.channel
        val map = ch.map(java.nio.channels.FileChannel.MapMode.READ_WRITE, 0, regionLen.toLong())
        map.order(ByteOrder.LITTLE_ENDIAN)
        fbFile = raf
        fbMap = map
        bitmap = Bitmap.createBitmap(fbW, fbH, Bitmap.Config.ARGB_8888)
        pxInts = IntArray(fbW * fbH)
        blitBuf = ByteArray(frameBytes)
        Log.i(TAG, "framebuffer: ${f.path} ${fbW}x${fbH} (${regionLen} B)")
    }

    private fun fbPath(): File = File(filesDir, FB_NAME)

    /** Extrae el binario del APK a filesDir con chmod 700 (patrón F0). */
    private fun installBinary(): File {
        val bin = File(filesDir, ASSET)
        if (bin.exists() && !bin.delete()) {
            throw IOException("no pude borrar ${bin.path}")
        }
        try {
            assets.open(ASSET).use { input ->
                bin.outputStream().use { output -> input.copyTo(output, 64 * 1024) }
            }
        } catch (e: IOException) {
            throw IOException(
                "asset '$ASSET' no encontrado — corre ./arca.sh build (copió devapp-demo a assets/)",
                e
            )
        }
        val ok = bin.setReadable(true, false) &&
            bin.setWritable(true, false) &&
            bin.setExecutable(true, false)
        if (!ok) throw IOException("chmod 700 falló sobre ${bin.path}")
        Log.i(TAG, "instalado: ${bin.path} (${bin.length()} B)")
        return bin
    }

    /** Lector de stdout del hijo: eventos → logcat + blit. */
    private fun pumpStdout(p: Process) {
        try {
            BufferedReader(InputStreamReader(p.inputStream, Charsets.UTF_8)).use { r ->
                while (true) {
                    val line = r.readLine() ?: break
                    handleLine(line)
                }
            }
        } catch (e: IOException) {
            Log.w(TAG, "demo stdout: ${e.message}")
        }
    }

    private fun handleLine(line: String) {
        val json = try {
            JSONObject(line)
        } catch (_: Exception) {
            Log.i(TAG, line)   // línea no-JSON (rara): log tal cual
            return
        }
        when (json.optString("event")) {
            "frame" -> {
                framesSeen++
                // pacing: 1 de cada 60 frames al logcat (evita spam)
                if (framesSeen % 60 == 1L) {
                    Log.i(TAG, "frame seq=${json.optLong("seq")} slot=${json.optInt("slot")}")
                }
                if (surfaceReady) blit()
            }
            "stats" -> {
                val msg = "stats: frames=${json.optLong("frames")} fps=${json.optLong("fps")}"
                Log.i(TAG, msg)
                postStatus(msg + " · blits: $framesBlit")
            }
            "hello" -> Log.i(TAG, "hijo listo: $line")
            "pong" -> Log.i(TAG, "pong seq=${json.optLong("seq")}")
            "exiting", "sigterm" -> Log.i(TAG, line)
            "fatal" -> {
                Log.e(TAG, "hijo FATAL: $line")
                postStatus("FAIL hijo: ${json.optString("error")}")
            }
            else -> Log.i(TAG, line)
        }
    }

    // ───────────────────── lectura seqlock + blit ─────────────────────

    /**
     * Lee el frame válido más nuevo (protocolo seqlock espejo de
     * FrameSlots::read_latest_into) y lo pinta en el SurfaceView.
     */
    private fun blit() {
        val map = fbMap ?: return
        val bmp = bitmap ?: return
        val payload = readLatest(map) ?: return   // sin frame nuevo válido

        // bitmap RGBA→ARGB ints (determinista, independiente del layout
        // interno de Bitmap: setPixels usa Color.argb)
        var at = HDR
        var i = 0
        val n = fbW * fbH
        while (i < n) {
            val r = payload[at].toInt() and 0xFF
            val g = payload[at + 1].toInt() and 0xFF
            val b = payload[at + 2].toInt() and 0xFF
            pxInts[i] = (0xFF shl 24) or (r shl 16) or (g shl 8) or b
            at += 4
            i++
        }
        bmp.setPixels(pxInts, 0, fbW, 0, 0, fbW, fbH)

        val canvas: Canvas = try {
            surfaceView.holder.lockCanvas() ?: return
        } catch (_: Exception) {
            return
        }
        try {
            canvas.drawColor(AColor.BLACK)
            canvas.drawBitmap(bmp, null, dstRect, paint)
        } finally {
            try {
                surfaceView.holder.unlockCanvasAndPost(canvas)
            } catch (_: Exception) {
            }
            framesBlit++
        }
    }

    /**
     * Seqlock del lado lector: prueba ambos slots, copia-y-revalida
     * (2 intentos por slot), se queda con el seq impar más alto.
     * Devuelve el buffer con el payload copiado o null si no hay frame.
     * Sin alocación por frame: usa `blitBuf`.
     */
    private fun readLatest(map: MappedByteBuffer): ByteArray? {
        val buf = blitBuf
        if (buf.size != frameBytes) return null
        var bestSeq = -1L
        var bestSlot = -1
        for (slot in 0 until SLOTS) {
            val base = slot * slotStride
            repeat(2) {
                val s1 = synchronized(map) { map.getLong(base) }
                if (s1 and 1L != 1L) return@repeat   // escribiendo/inválido
                synchronized(map) {
                    map.position(base + SLOT_HDR)
                    map.get(buf, 0, frameBytes)
                }
                val s2 = synchronized(map) { map.getLong(base) }
                if (s1 == s2 && s2 and 1L == 1L && s2 > bestSeq) {
                    bestSeq = s2
                    bestSlot = slot
                }
            }
        }
        if (bestSlot < 0) return null
        // valida cabecera (magic + geometría): fail-closed
        if (buf[0] != 'A'.code.toByte() || buf[1] != 'F'.code.toByte() ||
            buf[2] != 'R'.code.toByte() || buf[3] != 'M'.code.toByte()
        ) {
            return null
        }
        val w = u16(buf, 8)
        val h = u16(buf, 10)
        if (w != fbW || h != fbH) return null
        return buf
    }

    private fun u16(b: ByteArray, at: Int): Int =
        (b[at].toInt() and 0xFF) or ((b[at + 1].toInt() and 0xFF) shl 8)

    /** Rect destino del blit: fit-center respetando aspecto del fb. */
    private fun computeDstRect(sw: Int, sh: Int) {
        if (sw <= 0 || sh <= 0 || fbW <= 0 || fbH <= 0) return
        val scale = minOf(sw.toFloat() / fbW, sh.toFloat() / fbH)
        val w = (fbW * scale).toInt()
        val h = (fbH * scale).toInt()
        dstRect.set(
            (sw - w) / 2, (sh - h) / 2,
            (sw - w) / 2 + w, (sh - h) / 2 + h
        )
    }

    // ───────────────────── touch → stdin del hijo ─────────────────────

    override fun onTouchEvent(ev: MotionEvent): Boolean {
        val out = childStdin ?: return false
        // pantalla → píxeles del framebuffer (dstRect escala)
        val x = ((ev.x - dstRect.left) * fbW / dstRect.width().coerceAtLeast(1)).toInt()
        val y = ((ev.y - dstRect.top) * fbH / dstRect.height().coerceAtLeast(1)).toInt()
        if (x < 0 || y < 0 || x >= fbW || y >= fbH) return true
        val phase = when (ev.actionMasked) {
            MotionEvent.ACTION_DOWN -> "down"
            MotionEvent.ACTION_MOVE -> "move"
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> "up"
            else -> return true
        }
        val t = System.currentTimeMillis()
        val msg = "{\"event\":\"touch\",\"phase\":\"$phase\",\"x\":$x,\"y\":$y,\"t\":$t}\n"
        try {
            synchronized(out) {
                out.write(msg.toByteArray(Charsets.UTF_8))
                out.flush()
            }
        } catch (e: IOException) {
            Log.w(TAG, "touch: hijo ya cerrado (${e.message})")
        }
        return true
    }

    // ───────────────────── cierre ─────────────────────

    /** Mata al hijo con SIGTERM (exit 0 limpio por contrato del demo). */
    private fun stopChild(reason: String) {
        val p = process ?: return
        Log.i(TAG, "stopChild($reason): destroy()")
        try {
            p.destroy()
            Thread { if (!p.waitFor(GRACE_S, TimeUnit.SECONDS)) p.destroyForcibly() }
                .also { it.name = "arca-demo-killer" }.start()
        } catch (_: Exception) {
        }
    }

    private fun postStatus(msg: String) {
        runOnUiThread {
            statusView.text = msg
            Log.i(TAG, msg)
        }
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()
}
