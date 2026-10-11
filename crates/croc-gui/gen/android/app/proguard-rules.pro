# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# 这两处没有 Java 侧的任何调用点，R8 只会把它们当死代码重命名或删掉，而运行时的
# 查找恰恰只按名字进行——删掉之后只在真机上表现为"发了没反应"。
#   CrocBridge：Rust 侧按类名 + 方法名 + JNI 描述符反查（crates/croc-gui/src/android.rs）。
#   SafPlugin：Tauri 的 PluginManager 按类名反射加载插件与其 @Command 方法。
-keep class cn.sumitm.crocw.CrocBridge { *; }
-keep class cn.sumitm.crocw.saf.SafPlugin { *; }