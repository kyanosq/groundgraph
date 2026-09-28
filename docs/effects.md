# 业务动作效果与写入者反查

先对目标仓库运行 `groundgraph index`，再查询：

```sh
groundgraph effects 'OrderService.confirm'         # 文本动作卡
groundgraph effects 'OrderService.confirm' --json  # 完整证据
groundgraph writers t_stock                        # 写该表的入口
groundgraph writers t_stock --json
```

`effects` 从指定符号沿 `trace` 的调用、实现、SQL 和持久化边取闭包。表操作 `read|insert|update|delete` 来自 SQL、Mapper XML 标签或 MyBatis-Plus 方法；不能确定时为 `unknown`。列仅在 SQL `SET` 或 Wrapper `set`/`setSql` 中可直接读出时显示。每条表证据带来源、断言状态和链路置信度；JSON 保留原始证据。HTTP、MQ 和事件效果由静态客户端调用或 Feign 注解识别，均为候选，不能证明请求实际发出。

事务组是 Spring 注解与静态调用链的可能上下文。类级 `@Transactional` 生效，`REQUIRES_NEW` 单列；同类内部调用绕过代理，不开启被调方法注解指定的事务。`noRollbackFor` 保留注解源码值。卡片提示事务内外部调用、吞异常的 `catch`、`@Async` 和 `@TransactionalEventListener` 边界。分支条件、真实 Bean 注入、回滚行为和运行时写集需要日志或运行证据复核。

未解析调用给出总数和前 30 个 `文件:行` 断点；文本卡片在省略时明确标注，`--json` 也保留同一 30 项上限与 `breakpoints_truncated`。`confirmed_effect_ratio` 是已确认的效果证据占全部表与外部效果证据的比例；其链路经过候选调用时不计为已确认。`truncated` 表示 `trace` 容量截断，不能把表或调用列表当完整结果。

`writers` 只从带明确写操作的 `persists_to` 边反查；读边不算写入。沿图的反向调用链寻找 HTTP、定时、事件监听和 MQ 监听入口。找不到入口时显示最上游可见方法，`method` 不等于已核实的外部入口。候选链路保持 `candidate`，结果的容量截断以 `truncated` 标注。旧图没有操作元数据时先重建索引；未标注的边不会被推定为写入。
