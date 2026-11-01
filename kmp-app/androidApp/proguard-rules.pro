# Release minify is currently off (androidApp/build.gradle.kts). These rules are what enabling it needs:
# JNA looks up classes, fields and methods reflectively and from native code (libjnidispatch.so), and the
# UniFFI bindings (uniffi.client_ffi) are JNA Library/Structure/Callback implementations accessed the same way.
-keep class com.sun.jna.** { *; }
-keep class * implements com.sun.jna.** { *; }
-keep class uniffi.** { *; }
-dontwarn java.awt.**
-dontwarn com.sun.jna.**
