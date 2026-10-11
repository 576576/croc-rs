# 本插件的 Kotlin 类由 JNI 按名字反射查找（tauri 的 PluginManager 也按名字查
# @TauriPlugin 类），混淆会把类名与 @Command 方法名改掉，运行时就找不到。
# consumer-rules.pro 会被打进依赖方的混淆配置，所以规则写在这里。
-keep class cn.sumitm.crocw.saf.** { *; }
