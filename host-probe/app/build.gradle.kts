// host-probe/app — módulo único del APK de probe (F0).
// Sin dependencias externas: sin AndroidX, sin compose, sin nada que
// aumente la superficie de build. Es un probe desechable de 1 botón.

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.arca.probe"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.arca.probe"
        minSdk = 26

        // LA GRIETA (blueprint docs/01 §2): con targetSdk 28 el proceso corre
        // en el dominio SELinux `untrusted_app_27`, que PERMITE execve() de
        // un ELF extraído en /data/data. Con 29+ (W^X) el exec fallaría con
        // EACCES. NO subir esto a 29+ sin haber activado el backend WASM
        // (docs/12, F5). Coste: no publicable en Play (irrelevante aquí).
        targetSdk = 28

        versionCode = 2
        versionName = "0.1.0-f3a.r10"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Sin signingConfig de release: para el probe basta el build
            // debug (firma automática con el keystore de depuración).
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    // Intencionalmente vacío (task T02: "sin dependencias externas").
}
