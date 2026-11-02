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
// runtime classpath. `sim` sees `main` (and :shared's UniFFI bridge); tests see `sim`.
val sim: SourceSet by sourceSets.creating
val simImplementation: Configuration by configurations.getting {
    extendsFrom(configurations.implementation.get())
}
configurations.named(sim.runtimeOnlyConfigurationName) {
    extendsFrom(configurations.runtimeOnly.get())
}

// The UniFFI bindings and the JNA-backed UniffiEngineBridge live in :shared's jvmCommonMain source set
// (DECISIONS D5); :shared also owns the buildRustFfi task, which its desktop compile depends on.
dependencies {
    implementation(project(":shared"))
    implementation(compose.desktop.currentOs)
    implementation(libs.compose.material3)
    implementation(libs.kotlinx.coroutines.swing)

    simImplementation(sourceSets.main.get().output)
    simImplementation(libs.compose.ui.test)

    testImplementation(kotlin("test"))
    testImplementation(sim.output)
    testImplementation(libs.compose.ui.test)
}

val repoRoot: File = rootDir.parentFile

// Rust cdylib (libclient_ffi.dylib) built by :shared:buildRustFfi (scripts/build_ffi.sh).
val rustLibDir = "${rootDir}/../target/release"

compose.desktop {
    application {
        mainClass = "org.theconstruct.desktop.MainKt"
        // Applies to `:desktopApp:run` and packaged distributions.
        jvmArgs += "-Djna.library.path=$rustLibDir"
        // `./gradlew :desktopApp:run -Dconstruct.baseUrl=...` sets the property in Gradle's JVM, not the app's: forward it.
        listOf("construct.baseUrl", "construct.workspace").forEach { key ->
            providers.systemProperty(key).orNull?.takeIf { it.isNotBlank() }?.let { jvmArgs += "-D$key=$it" }
        }
        nativeDistributions {
            targetFormats(TargetFormat.Dmg)
            packageName = "The Construct"
            packageVersion = "1.0.0"
            // App icon "the construct" (scripts/gen_icons.sh regenerates these from assets/icon).
            macOS {
                bundleID = "org.theconstruct.desktop"
                dockName = "The Construct"
                iconFile.set(repoRoot.resolve("assets/icon/construct.icns"))
            }
            windows { iconFile.set(repoRoot.resolve("assets/icon/construct.ico")) }
            linux { iconFile.set(repoRoot.resolve("assets/icon/png/construct-512.png")) }
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

// Headless simulator: renders ConstructApp offscreen and writes PNG screenshots driven by a script.
// Relative paths (-Pfixture, -Pout, -PscriptFile, -Pstorage) resolve against the repo root (the working dir).
//   ./gradlew -PskipRustBuild :desktopApp:simulate -Pworkspace=home -Pscreen=phone \
//       -Pscript="shot start; click Increment; assert-text Count: 1; shot after"
val simulate by tasks.registering(JavaExec::class) {
    group = "verification"
    description = "Renders the shared Compose UI headlessly and writes screenshots (see README: Simulation mode)"
    classpath = sim.runtimeClasspath
    mainClass = "org.theconstruct.desktop.sim.SimulatorKt"
    workingDir = repoRoot
    jvmArgs("-Djava.awt.headless=true")
    systemProperty("construct.sim.defaultOutRoot", layout.buildDirectory.dir("sim").get().asFile.absolutePath)
    // Forward -P<name>=<value> as --<name>=<value>.
    val names = listOf("workspace", "screen", "script", "scriptFile", "fixture", "baseUrl", "mode", "out", "density", "timeoutMs", "storage", "pixelfedLogin")
    // providers.gradleProperty reads only -P / gradle.properties (project.findProperty("script")
    // would pick up an unrelated built-in project property).
    val forwarded = names.mapNotNull { n -> providers.gradleProperty(n).orNull?.let { "--$n=$it" } }
    args(forwarded)
    // Warn about likely typos (e.g. -Pscren): any other -P is silently ignored. Computed at
    // configuration time; the doFirst only captures a List<String> (configuration-cache safe).
    val known = names + "skipRustBuild"
    val unknown = gradle.startParameter.projectProperties.keys
        .filter { it !in known && !it.startsWith("org.gradle") && !it.startsWith("kotlin") && !it.startsWith("android") }
        .sorted()
    doFirst {
        unknown.forEach { System.err.println("WARNING: unknown -P property '$it' ignored by simulate (known: ${known.joinToString()})") }
    }
    // Always rerun; outputs are timestamped.
    outputs.upToDateWhen { false }
}
