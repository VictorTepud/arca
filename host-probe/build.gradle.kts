// host-probe — build raíz.
// AGP 8.5.2 requiere Gradle 8.7+ (sugerido: 8.9) y JDK 17.
// Kotlin puro, sin AndroidX: el APK usa solo el framework de Android.

plugins {
    id("com.android.application") version "8.5.2" apply false
    id("org.jetbrains.kotlin.android") version "1.9.24" apply false
}
