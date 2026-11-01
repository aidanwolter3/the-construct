import org.jetbrains.compose.desktop.application.dsl.TargetFormat

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.compose.multiplatform)
}

kotlin {
    jvmToolchain(21)
}

// UniFFI-generated Kotlin bindings (package `uniffi.client_ffi`) go in
// desktopApp/src/main/kotlin/uniffi/client_ffi/. UniFFI's Kotlin backend loads
// the Rust cdylib through JNA, so the bindings are JVM-only and live here rather
// than in :shared's commonMain. They implement org.genui.canvas.CanvasEngineBridge.
dependencies {
    implementation(project(":shared"))
    implementation(compose.desktop.currentOs)
    implementation(libs.compose.material3)
    implementation(libs.kotlinx.coroutines.swing)
    implementation(libs.jna)
    testImplementation(kotlin("test"))
}

// Rust cdylib (libclient_ffi.dylib) built by `cargo build --release -p client-ffi`.
val rustLibDir = "${rootDir}/../target/release"

compose.desktop {
    application {
        mainClass = "org.genui.canvas.desktop.MainKt"
        // Applies to `:desktopApp:run` and packaged distributions.
        jvmArgs += "-Djna.library.path=$rustLibDir"
        nativeDistributions {
            targetFormats(TargetFormat.Dmg)
            packageName = "ProjectCanvas"
            packageVersion = "1.0.0"
        }
    }
}

// Belt and braces: also set it on every JavaExec (run, hot-reload runs, etc.).
tasks.withType<JavaExec>().configureEach {
    systemProperty("jna.library.path", rustLibDir)
}
tasks.withType<Test>().configureEach {
    systemProperty("jna.library.path", rustLibDir)
    testLogging { showStandardStreams = true }
}

// Builds the Rust cdylib and regenerates the UniFFI Kotlin bindings before compiling.
// Skip with -PskipRustBuild (e.g. when the bindings/dylib are already up to date).
val repoRoot = rootDir.parentFile
val buildRustFfi by tasks.registering(Exec::class) {
    group = "build"
    description = "Builds client-ffi (cargo) and generates Kotlin bindings via scripts/build_ffi.sh"
    onlyIf { !project.hasProperty("skipRustBuild") }
    workingDir = repoRoot
    commandLine("bash", File(repoRoot, "scripts/build_ffi.sh").absolutePath)
    // Gradle (esp. the daemon) may not inherit a login-shell PATH containing cargo.
    val cargoBin = "${System.getProperty("user.home")}/.cargo/bin"
    environment("PATH", "$cargoBin${File.pathSeparator}${System.getenv("PATH") ?: ""}")

    inputs.files(
        fileTree(File(repoRoot, "crates")) { include("**/src/**", "**/Cargo.toml") },
        File(repoRoot, "Cargo.toml"),
        File(repoRoot, "Cargo.lock"),
        File(repoRoot, "scripts/build_ffi.sh"),
    ).withPropertyName("rustSources")
    outputs.file(File(repoRoot, "target/release/libclient_ffi.dylib"))
    outputs.file(file("src/main/kotlin/uniffi/client_ffi/client_ffi.kt"))
}

tasks.named("compileKotlin") {
    dependsOn(buildRustFfi)
}
