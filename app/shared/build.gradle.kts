import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.plugin.KotlinPlatformType
import org.jetbrains.kotlin.gradle.tasks.KotlinCompilationTask

plugins {
    alias(libs.plugins.kotlin.multiplatform)
    alias(libs.plugins.android.kotlin.multiplatform.library)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.compose.multiplatform)
}

kotlin {
    jvmToolchain(21)

    jvm("desktop")

    // Android library target via the KMP-native AGP plugin (DECISIONS D10).
    android {
        namespace = "org.theconstruct.shared"
        compileSdk = 36
        minSdk = 26
    }

    // Source set layout (DECISIONS D5):
    //   commonMain            models, renderer, ConstructApp, ConstructEngineBridge interface
    //   └─ jvmCommonMain      UniFFI bindings (uniffi.client_ffi) + UniffiEngineBridge (JNA)
    //      ├─ desktopMain     JNA jar, stdout logging
    //      └─ androidMain     JNA aar (bundles libjnidispatch.so), logcat logging
    @OptIn(ExperimentalKotlinGradlePluginApi::class)
    applyDefaultHierarchyTemplate {
        common {
            group("jvmCommon") {
                withJvm()
                // withAndroidTarget() only matches the legacy androidTarget(); the KMP Android
                // library plugin's target is matched by platform type.
                withCompilations { it.target.platformType == KotlinPlatformType.androidJvm }
            }
        }
    }

    sourceSets {
        commonMain.dependencies {
            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(libs.compose.material3)
            implementation(compose.ui)
            // AsyncImage. `api` so the apps and the simulator can configure the ImageLoader.
            api(libs.coil.compose)
            api(libs.kotlinx.serialization.json)
            api(libs.kotlinx.coroutines.core)
        }
        commonTest.dependencies {
            implementation(kotlin("test"))
        }
        // JNA is added per platform, never in jvmCommonMain: desktop needs the plain jar, Android
        // the aar (the jar on Android would duplicate classes and lack the Android natives).
        // `api` because the generated bindings expose JNA types (Pointer) in public signatures.
        // Coil's OkHttp network fetcher (desktop JVM + Android), auto-registered via ServiceLoader.
        named("jvmCommonMain").dependencies {
            api(libs.coil.network.okhttp)
        }
        named("desktopMain").dependencies {
            api(libs.jna)
        }
        androidMain.dependencies {
            api("net.java.dev.jna:jna:${libs.versions.jna.get()}@aar")
            // BackHandler (PlatformBackHandler's Android actual).
            implementation(libs.androidx.activity.compose)
        }
    }
}

// Builds the Rust cdylib (macOS) and regenerates the UniFFI Kotlin bindings
// (src/jvmCommonMain/kotlin/uniffi/client_ffi/client_ffi.kt, committed) before the desktop compile.
// Android compiles only need the committed bindings, so they don't depend on this task (they only
// order after it, in case both run in one build). Skip with -PskipRustBuild.
val repoRoot = rootDir.parentFile
val buildRustFfi by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds client-ffi (cargo) and generates Kotlin bindings via scripts/build_ffi.sh"
    // Resolved at configuration time: Task.project at execution time breaks the configuration cache.
    val skipRustBuild = project.hasProperty("skipRustBuild")
    onlyIf { !skipRustBuild }
    workingDir = repoRoot
    commandLine("bash", File(repoRoot, "scripts/build_ffi.sh").absolutePath)
    // Gradle (esp. the daemon) may not inherit a login-shell PATH containing cargo.
    val cargoBin = "${System.getProperty("user.home")}/.cargo/bin"
    environment("PATH", "$cargoBin${File.pathSeparator}${System.getenv("PATH") ?: ""}")

    inputs.files(
        fileTree(File(repoRoot, "crates")) {
            include("**/src/**", "**/Cargo.toml", "**/build.rs")
            exclude("**/.*.swp")
        },
        File(repoRoot, "Cargo.toml"),
        File(repoRoot, "Cargo.lock"),
        File(repoRoot, "scripts/build_ffi.sh"),
    ).withPropertyName("rustSources")
    outputs.file(File(repoRoot, "target/release/libclient_ffi.dylib"))
    outputs.file(file("src/jvmCommonMain/kotlin/uniffi/client_ffi/client_ffi.kt"))
}

tasks.withType<KotlinCompilationTask<*>>().configureEach {
    if (name == "compileKotlinDesktop") dependsOn(buildRustFfi) else mustRunAfter(buildRustFfi)
}
