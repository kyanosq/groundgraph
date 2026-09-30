# Java 字符串 ID 分派候选

本适配器处理显式配置的 Java command bus / capability registry 约定。它只解析源码，不执行仓库代码、不连接业务数据库，也不调用模型。

## 配置

在 `java_semantics.dispatch_contracts` 中按部署作用域声明约定。字段如下：

- `name`：本次约定的唯一名称。
- `dispatcher_methods`：经 javac 解析的全限定声明类和方法名，如 `sample.Bus.invoke`。
- `registration_methods`：创建 handler 的工厂方法，如 `sample.Factory.command`。
- `handler_type`：注册方法声明的精确返回类型，如 `sample.Handler`。
- `registration_annotation`：可选，注册方法必须有的完整注解类型，如 `sample.Register`。

默认没有任何约定，不猜测某个 `invoke` 或 `command` 名字的业务意义。约定只支持首个参数是 String ID。文字字符串、编译期字符串常量和有限的字符串拼接可以定位；运行时配置、方法计算、动态拼装、转发参数保持未解析。

## 证据与图语义

javac 证明调用目标和常量值。返回语句中的工厂调用，在其所属方法返回指定 handler 类型、且满足可选注解要求时，构成静态注册候选。

分派到注册作用域的图边是 `References`，始终为 `candidate/proposed`，不是已发生的调用。目标是包含工厂及回调声明的方法，**不是运行期 callback 的精确入口**。证据保留调用点、源码片段、注册位置、约定名称和原始 ID。同一行重复调用仍保留两个调用点。

注册方法体内的回调调用图可以提供后续探索线索，但不能把它的所有分支都视为一定执行。置信度沿用现有候选分数约定，不是经统计校准的正确概率。

## 明确保留的未知

- `dynamic_dispatch_id`：分派 ID 不能静态求值。
- `dynamic_registration_id`：已识别注册形式但 ID 动态。
- `dispatch_registration_missing`：在配置和索引范围内没有找到匹配注册；不等于运行时能力不存在。
- `ambiguous_dispatch_registration`：多个候选注册，全部保留，不任意选择一个。

调用本身编译失败/接收者未知时，继续在原 `java_analysis.calls` 清单中显示未解析。适配器不会用同名匹配升级它。

当前范围不解析 Spring profile/条件装配、运行时 DI、代理、别名注册、工厂参数转发、跨进程路由或事务传播。不同部署/客户请分别建图和配置，不把共享能力 ID 的独立系统混成一个注册空间。首次样板只接 Java ID 分派，BPMN、DMN、完整事件映射是后续工作。

## 验证与使用

先运行 `cargo test -p groundgraph-engine --test java_dispatch`。合成夹具覆盖常量 ID、重复调用、动态 ID、同名假接收者、重复注册、缺失注解、非返回工厂调用、动态注册、默认关闭和重新索引去掉旧边。原 Java 语义回归另运行 `--test java_semantics`。

项目配置和客户证据输出留在客户工作区；不要把其源码、配置、索引或工作包复制进 GroundGraph 公共仓库。业务候选继续走现有 `BusinessCandidate` 与人工审核流程，不因新增这条图边自动成为已接受业务描述。
