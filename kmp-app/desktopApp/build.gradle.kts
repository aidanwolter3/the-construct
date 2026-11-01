import org.jetbrains.compose.desktop.application.dsl.TargetFormat

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.compose.multiplatform)
}

kotlin {
    jvmToolchain(21)
}

// Simulation mode (README "Simulation mode", DECISIONS D8): a separate `sim` source set holds the
// headless simulator and fake engine, so the Compose UI test harness stays off the shipping app's
// runtime classpath. `sim` sees `main` (EngineBridge, UniFFI bindings); tests see `sim`.
val sim: SourceSet by sourceSets.creating
val simImplementation: Configuration by configurations.getting {
    extendsFrom(configurations.implementation.get())
}
configurations.named(sim.runtimeOnlyConfigurationName) {
    extendsFrom(configurations.runtimeOnly.get())
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

    simImplementation(sourceSets.main.get().output)
    simImplementation(libs.compose.ui.test)

    testImplementation(kotlin("test"))
    testImplementation(sim.output)
    testImplementation(libs.compose.ui.test)
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
    systemProperty("java.awt.headless", "true")
    testLogging { showStandardStreams = true }
}

// Builds the Rust cdylib and regenerates the UniFFI Kotlin bindings before compiling.
// Skip with -PskipRustBuild (e.g. when the bindings/dylib are already up to date).
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

// Headless simulator: renders CanvasApp offscreen and writes PNG screenshots driven by a script.
// Relative paths (-Pfixture, -Pout, -PscriptFile) resolve against the repo root (the working dir).
//   ./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=home -Pscreen=phone \
//       -Pscript="shot start; click Increment; assert-text Count: 1; shot after"
val simulate by tasks.registering(JavaExec::class) {
    group = "verification"
    description = "Renders the shared Compose UI headlessly and writes screenshots (see README: Simulation mode)"
    classpath = sim.runtimeClasspath
    mainClass = "org.genui.canvas.desktop.sim.SimulatorKt"
    workingDir = repoRoot
    jvmArgs("-Djava.awt.headless=true")
    systemProperty("canvas.sim.defaultOutRoot", layout.buildDirectory.dir("sim").get().asFile.absolutePath)
    // Forward -P<name>=<value> as --<name>=<value>.
    val names = listOf("workspace", "screen", "script", "scriptFile", "fixture", "baseUrl", "mode", "out", "density", "timeoutMs")
    // providers.gradleProperty reads only -P / gradle.properties (project.findProperty("script")
    // would pick up an unrelated built-in project property).
    val forwarded = names.mapNotNull { n -> providers.gradleProperty(n).orNull?.let { "--$n=$it" } }
    args(forwarded)
    // Always rerun; outputs are timestamped.
    outputs.upToDateWhen { false }
}
