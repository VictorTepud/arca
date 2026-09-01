// host-probe — configuración Gradle (Kotlin DSL).
// Scaffold del APK de la fase F0 (tarea T02). No compila en este PC Linux
// (sin Android SDK): el usuario lo compila en Deepin con JDK 17 + SDK/NDK.
// Ver README.md (raíz de host-probe) para los pasos exactos.

pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositoriesMode.set(RepositoriesMode.FAIL_ON_PROJECT_REPOS)
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "host-probe"

include(":app")
