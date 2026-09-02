package dev.arca.probe

import android.app.Activity
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color as AColor
import android.graphics.Paint
import android.graphics.Rect
import android.os.Bundle
import android.util.Log
import android.view.Gravity
import android.view.MotionEvent
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.Toast
import java.io.BufferedReader
import java.io.File
import java.io.IOException
import java.io.InputStreamReader
import java.io.OutputStream
import java.io.RandomAccessFile
import java.nio.ByteOrder
import java.nio.MappedByteBuffer
import java.util.concurrent.TimeUnit
import org.json.JSONObject
import kotlin.concurrent.thread

/**
 * Probe visual F3a — el "display server" de juguete del proyecto.
 *
 * r11: SIN inmersiva. La barra de notificaciones queda VISIBLE (pedido
 * explícito del usuario tras r10) y el framebuffer se dimensiona con la
 * superficie REAL del SurfaceView — que el window ya recorta bajo la
 * status bar: geometría correcta "restando la barra" de verdad, no con
 * displayMetrics de pantalla completa (que era la causa del pixelado y
 * del texto en miniatura de r10).
 *
 * El binario a lanzar llega SIEMPRE en el extra [EXTRA_BIN] (el lanzador
 * MainActivity lo copia a filesDir/exec con chmod; la demo incorporada del
 * APK se retiró en r11 — el usuario ya carga sus binarios con el botón +).
 *
 * Flujo (idéntico al diagrama graphs/gfx-f3a.mmd):
 *  1. surfaceChanged entrega el tamaño real de la vista → fb (lado ≤
 *     MAX_LADO, pares) → `filesDir/arca-fb.bin` con la geometría EXACTA
 *     del double-buffer seqlock de arca-shm.
 *  2. Mapea el archivo RW (MAP_SHARED — coherente con el mmap del hijo).
 *  3. Lanza el binario con fork+exec: mismo UID, mismo sandbox.
 *  4. Hilo lector de stdout: `{"event":"frame"}` → lee el frame más novo
 *     con el protocolo seqlock y lo blitea al SurfaceView (bilinear).
 *  5. Touch → stdin del hijo como JSON (pantalla→píxeles del fb).
 *  6. El hijo muere (X de la sub-app, watchdog o señal) → log + finish()
 *     → vuelve al lanzador.
 *
 * Layout del payload de cada slot (espejo de arca-gfx-protocol):
 *  0..4 "AFRM" · 4 version(=1) · 5 formato(1=RGBA) · 7 flags ·
 *  8..10 w(u16 LE) · 10..12 h(u16 LE) · 12..16 frame_seq(u32) ·
 *  16..24 ts_ms(u64) · 24..32 cero · después bitmap RGBA top-down.
 */
class DemoActivity : Activity() {

    private lateinit var surfaceView: SurfaceView

    // Binario a lanzar (obligatorio desde r11: sin demo incorporada).
    private var binPath: String? = null

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

    // r11: arranque diferido al primer surfaceChanged (solo hilo UI).
    private var started = false

    private var framesBlit = 0L
    private var framesSeen = 0L

    private companion object {
        private const val TAG = "ArcaProbe"
        private const val EXTRA_BIN = "bin"
        private const val FB_NAME = "arca-fb.bin"
        private const val SLOT_HDR = 16        // arca-shm: seq u64 + pad 8
        private const val SLOTS = 2            // double-buffer
        private const val HDR = 32             // arca-gfx-protocol
        // r9: watchdog 900 s (antes 180 cortaba la demo a los ~5401
        // frames con un exit 0 tan limpio que parecía crash).
        private const val WATCHDOG_S = 900L
        private const val GRACE_S = 3L
        // r11: el fb sale de la SUPERFICIE REAL de la vista (que el window
        // ya deja bajo la barra de notificaciones), no de displayMetrics
        // de pantalla completa. En un FHD típico: vista ~1080×2220 → fb
        // ~1049×2160 → el blit escala 1.03× (en r10 era 1.6×: pixelado).
        // Cap para acotar el costo (render CPU del hijo + bucle RGBA→ARGB
        // de Kotlin): en QHD limita a ~1.4×; si un teléfono no llega a
        // 30 fps con fb 2160, BAJA esta constante.
        private const val MAX_LADO = 2160      // presupuesto de blit
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // r9: sin esto el timeout de pantalla apaga el SurfaceView a mitad
        // de la sesión y EMUI puede matar el proceso en segundo plano.
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        // r11: SIN applyImmersive: el usuario pidió MANTENER la barra de
        // notificaciones; el tema Theme.ArcaView ya no es Fullscreen y el
        // contenido se recorta bajo la status bar automáticamente.

        binPath = intent.getStringExtra(EXTRA_BIN)

        surfaceView = SurfaceView(this).apply {
            holder.addCallback(object : SurfaceHolder.Callback {
                override fun surfaceCreated(h: SurfaceHolder) {
                    surfaceReady = true
                }

                // r11: AQUÍ llega la geometría verdadera (pantalla menos
                // status bar). El primer evento dimensiona el fb y arranca
                // al hijo; los siguientes solo re-centran el blit.
                //
                // r12: ORDEN CRÍTICO — computeDstRect DESPUÉS de
                // dimensionarFb. En r11 se llamaba antes: con fb 0×0 el
                // guard de computeDstRect saltaba y dstRect quedaba
                // (0,0,0,0) PARA SIEMPRE → drawBitmap a un rect vacío no
                // pinta NADA → pantalla negra con todos los logs sanos
                // (hello, 30 fps, blits contando). detectado en hardware
                // por el usuario; el harness qemu no puede verlo porque
                // prueba al hijo, no la lógica del host.
                override fun surfaceChanged(h: SurfaceHolder, format: Int, w: Int, h2: Int) {
                    if (!started && w > 0 && h2 > 0) {
                        started = true
                        dimensionarFb(w, h2)
                        computeDstRect(w, h2)   // ya con fbW/fbH reales
                        running = true
                        thread(name = "arca-demo") { runDemo() }
                    } else {
                        // cambios posteriores (rotación, relayout):
                        // re-centrar el blit con la geometría nueva.
                        computeDstRect(w, h2)
                    }
                }

                override fun surfaceDestroyed(h: SurfaceHolder) {
                    surfaceReady = false
                }
            })
        }
        val root = FrameLayout(this).apply {
            addView(
                surfaceView,
                FrameLayout.LayoutParams(
                    FrameLayout.LayoutParams.MATCH_PARENT,
                    FrameLayout.LayoutParams.MATCH_PARENT,
                    Gravity.CENTER
                )
            )
        }
        setContentView(root)
    }

    // ───────────────────── ciclo del visor ─────────────────────

    private fun runDemo() {
        try {
            setupFramebuffer()
            val bin = resolveBinary() ?: return   // ya avisó con toast
            Log.i(TAG, "lanzando: ${bin.absolutePath} (fb ${fbW}x${fbH})")

            val pb = ProcessBuilder(bin.absolutePath)
            pb.redirectErrorStream(true)
            pb.environment()["ARCA_FB"] = fbPath().absolutePath
            pb.environment()["ARCA_FB_W"] = fbW.toString()
            pb.environment()["ARCA_FB_H"] = fbH.toString()
            val p = try {
                pb.start()
            } catch (e: IOException) {
                // binario externo roto: ENOEXEC/ENOENT/EACCES — típico si
                // el archivo elegido no era un devapp aarch64.
                toast(getString(R.string.toast_exec_fail, e.message))
                throw e
            }
            process = p
            childStdin = p.outputStream

            thread(name = "arca-demo-stdout") { pumpStdout(p) }

            // Watchdog de seguridad (15 min: margen de sobra para usar la
            // sub-app; sigue acotando un hijo colgado).
            val finished = p.waitFor(WATCHDOG_S, TimeUnit.SECONDS)
            if (!finished) {
                Log.i(TAG, "watchdog: destroy() → SIGTERM")
                p.destroy()
                if (!p.waitFor(GRACE_S, TimeUnit.SECONDS)) {
                    p.destroyForcibly()
                    p.waitFor()
                }
            }
            val code = p.exitValue()
            Log.i(TAG, "exit code = $code (blits: $framesBlit/${framesSeen} frames)")
        } catch (t: Throwable) {
            Log.e(TAG, "demo FAILED", t)
        } finally {
            running = false
            // El hijo terminó (X de la sub-app, shutdown del host o
            // watchdog) → cerrar el visor y volver al lanzador. Si la
            // activity ya se está cerrando (onDestroy), no hacemos nada.
            runOnUiThread { if (!isFinishing) finish() }
        }
    }

    /**
     * Binario a ejecutar: SOLO el del extra [EXTRA_BIN] (r11: la demo
     * incorporada se retiró — todo pasa por el lanzador o ./arca.sh run).
     * Devuelve null (con toast) si falta o es inaccesible.
     */
    private fun resolveBinary(): File? {
        val extra = binPath
        if (extra.isNullOrBlank()) {
            toast(getString(R.string.toast_no_binary))
            Log.e(TAG, "sin extra 'bin' — ábrela desde el lanzador")
            return null
        }
        val f = File(extra)
        if (f.isFile && f.canRead()) {
            // defensivo: refresca el bit +x (la grieta targetSdk 28
            // permite exec en /data/data, pero el bit de modo importa)
            f.setExecutable(true, false)
            Log.i(TAG, "binario: ${f.path} (${f.length()} B)")
            return f
        }
        toast(getString(R.string.toast_no_binary_file, extra))
        Log.e(TAG, "binario inaccesible: $extra")
        return null
    }

    /**
     * r11: dimensiona el fb desde la SUPERFICIE real de la vista (que ya
     * excluye la status bar), con tope MAX_LADO en el lado mayor y valores
     * pares (u16 friendly del protocolo).
     */
    private fun dimensionarFb(sw: Int, sh: Int) {
        val scale = minOf(1f, MAX_LADO.toFloat() / maxOf(sw, sh))
        fbW = ((sw * scale).toInt() / 2 * 2).coerceAtLeast(2)
        fbH = ((sh * scale).toInt() / 2 * 2).coerceAtLeast(2)
        Log.i(TAG, "vista ${sw}x${sh} → fb ${fbW}x${fbH} (escala de blit " +
            "%.3f)".format(maxOf(sw.toFloat() / fbW, sh.toFloat() / fbH)))
    }

    /** Crea el archivo de región + su mapeo (rol HOST). */
    private fun setupFramebuffer() {
        frameBytes = HDR + fbW * fbH * 4
        slotStride = SLOT_HDR + frameBytes
        val regionLen = SLOTS * slotStride

        val f = File(filesDir, FB_NAME)
        if (f.exists() && !f.delete()) {
            throw IOException("no pude borrar ${f.path} (¿sesión anterior viva?)")
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
            Log.w(TAG, "stdout: ${e.message}")
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
                // pacing: 1 de cada 61 frames al logcat (evita spam). 61 es
                // COPRIMO con los 2 slots del double-buffer → el slot
                // logueado ALTERNa 0/1/0/1 y el logcat DEMUESTRA la rotación.
                if (framesSeen % 61 == 1L) {
                    Log.i(TAG, "frame seq=${json.optLong("seq")} slot=${json.optInt("slot")}")
                }
                if (surfaceReady) blit()
            }
            "stats" -> Log.i(
                TAG,
                "stats: frames=${json.optLong("frames")} fps=${json.optLong("fps")} · blits: $framesBlit"
            )
            "hello" -> Log.i(TAG, "hijo listo: $line")
            "pong" -> Log.i(TAG, "pong seq=${json.optLong("seq")}")
            "exiting", "sigterm" -> Log.i(TAG, line)
            "fatal" -> {
                Log.e(TAG, "hijo FATAL: $line")
                toast(getString(R.string.toast_child_fail, json.optString("error")))
            }
            else -> Log.i(TAG, line)
        }
    }

    // ───────────────────── lectura seqlock + blit ─────────────────────

    /**
     * Lee el frame válido más novo (protocolo seqlock espejo de
     * FrameSlots::read_latest_into) y lo pinta en el SurfaceView.
     */
    private fun blit() {
        val map = fbMap ?: return
        val bmp = bitmap ?: return
        // r12: autodefensa — si por la razón que fuera el rect nunca se
        // calculó (callback perdido), se reconstruye con la geometría
        // REAL del holder en vez de blitear a un rect vacío (negro).
        if (dstRect.isEmpty) {
            val sf = surfaceView.holder.surfaceFrame
            computeDstRect(sf.width(), sf.height())
            if (dstRect.isEmpty) return   // sin superficie útil todavía
        }
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
        // r12: tripa al logcat — si esto imprime un rect vacío el blit
        // no va a pintar nada (la pantalla negra de r11 se veía aquí
        // al instante: "blit dst=Rect(0, 0 - 0, 0)").
        Log.i(TAG, "blit dst=$dstRect (fb ${fbW}x${fbH} en vista ${sw}x${sh})")
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

    override fun onDestroy() {
        running = false
        stopChild("activity destruida")
        super.onDestroy()
    }

    private fun toast(msg: String) {
        runOnUiThread {
            Toast.makeText(this, msg, Toast.LENGTH_LONG).show()
            Log.i(TAG, msg)
        }
    }
}
