package dev.arca.probe;

import android.app.Activity;
import android.graphics.Color;
import android.graphics.Typeface;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.util.Log;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.ScrollView;
import android.widget.TextView;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.text.SimpleDateFormat;
import java.util.Date;
import java.util.Locale;

/**
 * Una sola pantalla: un boton "ejecutar sonda" + una consola con los
 * resultados. Toda la logica esta en {@link NativeHost}; esta actividad
 * solo muestra, guarda y publica el registro.
 */
public class MainActivity extends Activity implements NativeHost.Logger {

    private TextView tv;
    private ScrollView scroll;
    private Button btn;
    private final Handler ui = new Handler(Looper.getMainLooper());
    private FileOutputStream logArchivo;
    private Thread trabajador;

    @Override
    protected void onCreate(Bundle estado) {
        super.onCreate(estado);
        construirPantalla();
        log("== Arca · sonda F0 (r3) en el telefono ==");
        log("Toca el boton para correr las 6 pruebas del motor.");
        log("Despues, en la PC: ./arca.sh logs  (guarda el registro).");
        if ("1".equals(getIntent().getStringExtra("auto"))) {
            ejecutar();
        }
    }

    private void construirPantalla() {
        LinearLayout raiz = new LinearLayout(this);
        raiz.setOrientation(LinearLayout.VERTICAL);
        raiz.setBackgroundColor(Color.parseColor("#101418"));

        btn = new Button(this);
        btn.setText("Ejecutar sonda F0");
        btn.setTextColor(Color.WHITE);
        btn.setBackgroundColor(Color.parseColor("#1E88E5"));
        btn.setOnClickListener(v -> ejecutar());
        raiz.addView(btn);

        tv = new TextView(this);
        tv.setTypeface(Typeface.MONOSPACE);
        tv.setTextSize(11f);
        tv.setTextColor(Color.parseColor("#D8DEE9"));
        tv.setPadding(32, 32, 32, 32);

        scroll = new ScrollView(this);
        scroll.setBackgroundColor(Color.parseColor("#101418"));
        scroll.addView(tv);
        raiz.addView(scroll, new LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.MATCH_PARENT));
        setContentView(raiz);
    }

    private void ejecutar() {
        if (trabajador != null && trabajador.isAlive()) {
            log("(la sonda ya esta corriendo...)");
            return;
        }
        btn.setEnabled(false);
        btn.setText("Corriendo...");
        trabajador = new Thread(() -> {
            new NativeHost(this, this).ejecutarSonda();
            ui.post(() -> {
                btn.setEnabled(true);
                btn.setText("Ejecutar sonda F0");
            });
        }, "arca-sonda");
        trabajador.start();
    }

    /**
     * Llega desde cualquier hilo: pantalla + logcat + archivo interno
     * (lo recupera "adb shell run-as dev.arca.probe cat files/arca-probe.log").
     */
    @Override
    public synchronized void log(String linea) {
        Log.i(NativeHost.TAG, linea);
        try {
            if (logArchivo == null) {
                File f = new File(getFilesDir(), "arca-probe.log");
                if (f.length() > 256 * 1024) {
                    File viejo = new File(getFilesDir(), "arca-probe-viejo.log");
                    //noinspection ResultOfMethodCallIgnored
                    viejo.delete();
                    //noinspection ResultOfMethodCallIgnored
                    f.renameTo(viejo);
                }
                logArchivo = new FileOutputStream(f, true);
                String marca = new SimpleDateFormat("yyyy-MM-dd HH:mm:ss", Locale.US)
                        .format(new Date());
                logArchivo.write(("---- " + marca + " ----\n").getBytes("UTF-8"));
            }
            logArchivo.write((linea + "\n").getBytes("UTF-8"));
            logArchivo.flush();
        } catch (IOException e) {
            Log.w(NativeHost.TAG, "no pude escribir el archivo de log", e);
        }
        ui.post(() -> {
            tv.append(linea + "\n");
            scroll.post(() -> scroll.fullScroll(ScrollView.FOCUS_DOWN));
        });
    }

    @Override
    protected void onDestroy() {
        synchronized (this) {
            try {
                if (logArchivo != null) {
                    logArchivo.close();
                    logArchivo = null;
                }
            } catch (IOException ignored) {
            }
        }
        super.onDestroy();
    }
}
