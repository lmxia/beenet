# Beenet —— 基于 CID 的分布式 Agent 任务网络

> 文档版本：**v2.13（M1 端到端闭环版 + Spin 对照纪要）**  
> 状态：M1 最短闭环已跑通（档 0 = Wasip2）；进入 M1.5 规划  
> 说明：v2.13 在 v2.12 基础上增加 §11.3（Spin 对照纪要）；v2.12 对应 M1 端到端可运行的工作树：worker 走 `wasmtime-wasi-http` 的 p2 `ProxyPre`，example 为 `spin-sdk 5.x` 的 `#[http_component]`。

---

## 0. TL;DR

Beenet 是一个“寻址即计算”的去中心化任务网络：

- **CID 即函数地址**：Wasm 二进制哈希是可执行逻辑的唯一标识。
- **P2P 即调用总线**：Gateway/Agent 通过 libp2p 调 Worker。
- **Wasm 即计算体**：毫秒级实例化、强隔离。
- **Factor 即能力边界**：能力最小化授权、审计/计量可观测。

两类入口：

- **P2P 原生入口**：Agent 直连 Worker。
- **HTTP 网关入口**：第三方通过 HTTPS 调 Gateway，由网关做协议转换与调度。

---

## 1. 目标与约束

### 1.1 功能目标

- 任务原子化与沙箱化。
- 动态内容寻址（CID）。
- 激活内网算力（libp2p + NAT/Relay 体系）。
- stdout/stderr 与资源使用量结构化回传（**M1.5**：`InvokeResponse` 加 `stdout`/`stderr` 字段；M1 仅 worker 本地日志）。

### 1.2 非功能目标

- 冷启动 P95 < 100ms（缓存命中更低）。
- 并发目标：单 Worker `CPU * 4` 级别。
- Gateway 无状态，可水平扩展。
- 默认安全姿态：不信任任意 CID，出网默认拒绝，文件默认不挂载。
  - **M1 实况**：默认只挂 stdio（见 D16）；文件系统默认未挂载已达成；出网默认拒绝在 M1 仅"不提供 `allowed_outbound_hosts` 入口"，`wasi:sockets` 的 linker 级钳制在 M1.5 随 OutboundNetworkingFactor 一起接入。

### 1.3 项目形态与依赖策略

- **D1**：独立仓库 + Host Embedding（不 fork Spin 主仓）。
- **D2**：Spin 核心依赖以 `git rev` 锁定（本地可 `[patch]` 覆盖为 path）。
- **D10**：pin 目标 `6d9e8c79...`（"bump Spin SDK to v6.0.0"），**但仅在 M1.5 开始引入 `spin-*` crate 时生效**。M1 当前 Cargo.toml 不依赖任何 `spin-*`，只用原生 `wasmtime` / `wasmtime-wasi` / `libp2p`，D10 暂为空承诺。

---

## 2. 总体架构

角色：

- **Gateway**：鉴权、限流、路由、转发、聚合审计。
- **Worker**：拉取/缓存 CID、执行 Wasm、回传结果。
- **Registry**（M2）：热路径元信息。
- **DHT**：冷路径发现。
- **Agent**：可绕过 Gateway 直接 P2P 调用 Worker。

---

## 3. 对 Spin 的三层扩展

### 3.1 加载层：`IpfsComponentLoader`

- 输入：CID。
- 输出：Wasm bytes -> `Component` / `InstancePre`。
- M1 可先本地文件伪 CID，M3 接 IPFS。

### 3.2 网络层：`P2pTrigger` + 执行器

协议：`/beenet/invoke/1.0`（Gateway/Agent -> Worker）。

执行器路线：

- **档 0（M1）**：走 `wasi:http/incoming-handler@0.2`（W3C 标准 wasi:http p2）。经验证：`spin-sdk 5.x` 的 `#[http_component]` 在 `wasm32-wasip2` target 下编译出的 component 原生导出该接口（`wasm-tools component wit` 输出 `export wasi:http/incoming-handler@0.2.0`），与 Spin 主仓 `trigger-http/src/wasi.rs` 管线同构。worker 侧直接用 `wasmtime_wasi_http::p2::bindings::Proxy` 即可；不必自写 WIT。
- **档 1（M3）**：`beenet:task/runner@0.1` 原生接口。

> 历史取舍：曾考虑走 `fermyon:spin/inbound-http` 老 WIT 作为 M1 最短闭环脚手架，因为 `handle-request: func(req) -> response` 是同步签名、宿主实现最省事。实测 spin-sdk 5.x 已整体迁到 wasi:http p2，老 WIT 在新 SDK 上完全不导出，因此废弃该路径。

### 3.3 能力层：`BeenetFactors`

- **D3**：必须扁平 `RuntimeFactors` 结构，不嵌套 `TriggerFactors`。
- 基线包含：`WasiFactor` / `VariablesFactor` / `OutboundNetworkingFactor` / `OutboundHttpFactor` + `AuditFactor`。
- **D5**：`NoFilesMounter`（默认不挂宿主目录）。
- **D6**：默认拒绝出网（manifest 申请，worker 再钳制）。

### 3.4 任务 WIT 与 SDK 路线

- **D7**：两档长期共存。
- M1 使用 Spin SDK 导出（档 0）。
- M3 发布 `beenet-sdk` 与 `#[beenet_task]`（档 1）。

### 3.5 任务打包：单文件分发

- **D12**：manifest 内嵌 `beenet:manifest/v1` custom section。
- `beenet-pack` 提供 build/inspect。
- **D13**：主来源是 custom section；本地 `policies.toml` 仅兜底；调用方不能随请求注入策略。

### 3.6 计费与计量（v2.9）

Wasmtime 计量源：

- fuel（指令级，确定性，M3 评估默认启用）
- epoch（仅限长，不作计费）
- cpu-time（真实 CPU 占用）
- memory-consumed（Wasm 线性内存）

三层账单：

```
bill = base_fee + compute_fee + resource_fee
```

- base：调用固定费
- compute：wall/cpu/fuel × chargeable_memory_mb
- resource：outbound_bytes / fd_writes / storage

边界规则（核心）：

- `Ok` / `BusinessError`：收
- `RuntimeError` / `Timeout(exec)`：免 base，收 compute/resource
- `LoadError` / `Rejected`：全免

`usage` 字段（v2.9）包含：

- `wall_ns`
- `cpu_ns`
- `fuel_used`
- `mem_bytes`
- `chargeable_memory_mb`
- `fd_writes`
- `outbound_bytes`
- `billable`

---

## 4. Gateway 设计

- 对外 API：`POST /run/ipfs/:cid`、`POST /invoke`、`GET /health`、`GET /metrics`
- 对内协议：`InvokeRequest` / `InvokeResponse`
- Status 体系：
  - `Ok`
  - `BusinessError`
  - `RuntimeError`
  - `LoadError { fetch | compile }`
  - `Timeout { gateway | exec }`
  - `Rejected`

---

## 5. 负载均衡策略（M2）

- 一致性哈希（按 CID）提高缓存命中。
- 过载回退到 least-inflight。
- 热点 CID 可做副本扩散与 hedged 请求（幂等前提下）。

---

## 6. Worker 实现与隔离

### 6.0 生命周期

- 启动期：构建 Engine、Factors、Executor、Trigger。
- 请求期：按 CID 查缓存、加载/编译、实例化执行、采集 usage。

### 6.1 缓存层级

- L1：`InstancePre` 级缓存（per CID）。
- L2：每请求 Store/Instance。
- L3：跨请求复用（M2/M3 评估）。

### 6.2 执行隔离硬上限

- Epoch deadline（wall-clock 限时）
- 内存上限（`max_memory_size` + StoreLimits）
- 出网白名单（OutboundNetworkingFactor）
- 并发上限（Worker Semaphore）
- 进程级资源（CPU/RSS）由 cgroup/k8s 兜底

**D14（内存双层）**：

- L1：启动期 Hook 进程硬顶
- L2：请求期按 manifest 精准覆盖
- `resolve_limits` 保证 L2 <= L1

---

## 7. 仓库骨架

已落地（M1）：

- `crates/beenet-common`：`BeenetCid`（CIDv1/raw/sha2-256）+ 协议常量。
- `crates/beenet-proto`：`InvokeRequest` / `InvokeResponse` / `Status` / `Usage`，libp2p CBOR 上线。
- `crates/beenet-manifest`：`beenet:manifest/v1` TOML schema + wasm custom section embed/extract。
- `crates/beenet-pack`：`build` / `inspect` CLI。
- `crates/beenet-worker`：`P2pTrigger` 雏形 = libp2p request-response + wasmtime 直连执行（档 0，`wasi:http/incoming-handler@0.2`）。
- `crates/beenet-gateway`：axum HTTP `POST /run/ipfs/:cid` → libp2p 转发。
- `examples/hello-filter-http`：`spin-sdk` 的 `#[http_component]` 示例任务。

规划中：

- `crates/beenet-registry`（M2）
- `crates/beenet-factors`（M1.5）：扁平 `BeenetFactors`，承载 Wasi/Variables/OutboundNetworking/OutboundHttp/Audit。
- `sdks/rust/beenet-sdk`（M3）
- `wit/beenet/task.wit`（M2.5 草案，M3 冻结）

---

## 8. 端到端示例（M1 档 0）

1. 任务作者使用 `#[http_component]`。
2. 写 `beenet.toml`。
3. `beenet-pack build` 输出 `task.wasm`。
4. 计算 CID 并发布。
5. Gateway/Agent 调用 `cid`。
6. Worker 执行并回传 body + usage。

---

## 9. Host Embedding 策略

- 不走 `spin up` CLI 壳。
- M1：可先动态 `LockedApp` / 或直接 wasmtime 路径打最短闭环。
- M2+：向完整 FactorsExecutor 管线靠拢（D4）。

---

## 10. 里程碑

- **M1（已闭环）**：最短闭环已端到端跑通（`POST /run/ipfs/<cid>` → gateway → libp2p → worker → wasi:http proxy → spin-sdk guest → body 回传）。单 worker、本地伪 CID（`./wasm_cache/<cid>.wasm`）、档 0（`wasi:http/incoming-handler@0.2`，走 `wasmtime-wasi-http` 的 p2 `ProxyPre`）、裸 HTTP gateway、无 Registry/LB。Gateway 通过 `BEENET_WORKER_ADDR=/ip4/.../tcp/.../p2p/<peerid>` 硬编码定位单 worker。
  - **M1 已完成**：`beenet-common` / `beenet-proto` / `beenet-manifest` / `beenet-pack`，libp2p request-response 通信骨架，gateway→worker 转发，`Status` A/B 表，`beenet-worker` 的 `TaskExecutor` trait 抽象 + `Wasip2HttpExecutor` 实现（档 0），worker 并发闸门（tokio `Semaphore`，默认 `available_parallelism * 4`，超闸门返回 `Status::Rejected`）。
  - **M1 已交付但仍有限制**：`InvokeResponse` 不回传 stdout/stderr（仅 worker 本地 tracing）、manifest `max_memory_mb` 只读不 apply、`deadline_ms` 走 `tokio::time::timeout`（非 wasmtime epoch）、出网钳制靠 `WasiCtx` 默认 capability-not-granted 兜底（见 D16）——这些都由 M1.5 正式化。
- **M1.5（下一步）**：
  - 接 `spin_factors_executor::FactorsExecutor::load_app`（动态 `LockedApp`，D4）。
  - 接 `BeenetFactors`（扁平 `RuntimeFactors`，D3）：`WasiFactor` / `VariablesFactor` / `OutboundNetworkingFactor` / `OutboundHttpFactor` + `AuditFactor`。
  - 让 D6（默认拒绝出网）在 linker 级真正生效（M1 仅靠"不主动挂 `wasi:sockets`"兜底，M1.5 由 OutboundNetworkingFactor 主动钳制）。
  - 接 `MaxInstanceMemoryHook` + `StoreLimits`，让 D14 L1/L2 真正 apply。
  - `InvokeResponse` wire 扩展 `stdout` / `stderr`（或可选 `log_blob_ref` 走外挂存储）。
  - pin 落实 D10 的 spin commit（首次真正引入 `spin-*` 依赖）。
- **M2**：多 worker + Registry + LB（一致性哈希 + least-inflight）。
- **M2.5**：`beenet:task/runner@0.1` WIT 草案（档 1 预览）。
- **M3**：IPFS Loader + 原生 `beenet-sdk`（档 1 正式）。
- **M4**：生产化（鉴权、计费、可观测、流式、fuel 计量）。

---

## 11. 决策记录

### 11.1 已决策（D1-D15）

- D1 项目形态：独立仓库 Host Embedding
- D2 依赖策略：git rev 统一 pin
- D3 Factor 组装：扁平 `BeenetFactors`
- D4 M1 加载路径：动态 LockedApp（后续迁 core 直连）
- D5 文件系统默认姿态：不挂载
- D6 出网默认姿态：默认拒绝
- D7 任务接口路线：档 0 + 档 1 共存
- D8 缓存策略：per-CID + LRU + singleflight
- D9 档 0 执行器：M1 直接走 `wasi:http/incoming-handler@0.2`（Wasip2HttpAdapter），宿主侧复用 `wasmtime-wasi-http` 提供的 p2 bindings。不再保留 `fermyon:spin/inbound-http` 兼容路径（spin-sdk 5.x 已不导出）
- D10 Spin commit pin：目标 `6d9e8c79...`，M1.5 首次引入 `spin-*` 依赖时生效
- D11 返回值语义：A/B 表映射
- D12 打包格式：单文件 + custom section
- D13 manifest 来源优先级：section 主、local policy 兜底
- D14 内存上限双层：L1 Hook + L2 handler（M1 仅读 manifest，M1.5 apply）
- D15 计费模型：三层账单 + 边界规则
- D16 M1 WASI 姿态：`WasiCtxBuilder` 仅 wire stdout/stderr，不调 `inherit_env` / `preopened_dir` / `inherit_network`，令 `wasi:sockets` / `wasi:filesystem` 即便在 linker 里也因 capability-not-granted 运行时拒绝。M1.5 改为 `OutboundNetworkingFactor` + `NoFilesMounter` 的可配置 allowlist

### 11.2 待定

- 幂等默认策略
- CID 白名单/签名策略
- Registry 一致性方案
- Worker 策略阈值实测
- 计费单价参数（运营）
- Fuel 档性能实测与 dry-run API 形态
- M1.5 并发闸门阈值默认值（候选 `CPU * 4`，见 §1.2）
- `InvokeResponse` 的 stdout/stderr wire 扩展：内联 vs `log_blob_ref`（超过阈值落外挂存储）

### 11.3 与 Spin `trigger-http` / `HandlerType` 对照（纪要）

- **Spin `handler_type` / `component_handler_types`**：`HttpServer::new` 阶段对每个 HTTP 组件取 `InstancePre`，经 `HandlerType::from_instance_pre` 扫描 **Wasm export**（wasi:http 各版本、`fermyon:spin/inbound-http` 等），得到**唯一**候选后写入 `HashMap<component_id, …>`；请求路径只做查表分派（见 `spin/crates/trigger-http/src/server.rs`、`spin/crates/http/src/trigger.rs`）。
- **Beenet _worker 当前形态**：`locked_app_single_http_component` + `wasmtime_wasi_http::p2::bindings::ProxyPre::new(instance_pre)`，**实质上固定为 wasi:http P2 `incoming-handler`（与 Spin 的 `Wasi0_2` / `ProxyIndices` 同族）**。不覆盖 Wagi、WASI HTTP 0.3、`fermyon:spin/inbound-http` 等分支时，**不必**复刻整张 `HandlerType` 枚举；若未来要「接任意 Spin 应用或多导出形态」，再考虑复用 `from_instance_pre` + `trigger-http/src/wasi.rs` 式分派。
- **`instantiate` 两次形态**：`FactorsInstanceBuilder::instantiate`（`factors-executor` 内已对 `instance_pre` 调用 `instantiate_async`）负责 **factors `Store` + 第一个 `Instance`**；Beenet 随后 `ProxyPre::instantiate_async(&mut store)` 是在**同一 `Store`** 上取 **typed `Proxy`** 以 `call_handle`，**不是**与前者同语义的重复。Spin `wasi.rs` 则在**同一** `instance` 上用 `indices.load(&mut store, &instance)`，故 **tuple 首元被显式使用**；Beenet 未走 `indices`，首元绑定为 `_instance` 主要为 **保活**（避免 `(_, store)` 使第一个 `Instance` 立即析构）。
- **`get_instance_pre` 与 `prepare` 所持 `instance_pre`**：同一套预编译结果的句柄引用/克隆，**非**二次从磁盘加载组件。

---

## 12. 参考锚点

- `spin/crates/trigger/src/lib.rs`
- `spin/crates/trigger-http/src/wasi.rs`
- `spin/crates/factors-executor/src/lib.rs`
- `spin/crates/core/src/store.rs`
- `spin/crates/core/src/limits.rs`

---

## 13. FAQ

1. 单任务能否多核？不能（任务内单线程，任务间并行）。
2. epoch 是 CPU 限制吗？不是，是 wall-clock 限时。
3. 每请求新建 Store 吗？M1 是，复用 M2/M3 再评估。
4. HTTP trigger 是否每请求一 task？本质是连接/执行模型，需并发闸门。
5. `max_memory_size` 是否等于 RSS？不是，RSS 需外部限制。
6. 为什么不直接 `spin up`？Beenet 是动态 CID 运行时。
7. 是否重写 Trigger 两个 doc-hidden hook？不建议，能力走 Factor。
8. 为什么自建 P2pTrigger？协议入口不同。
9. manifest 在哪里？Wasm custom section。
10. 作者 DX 是否兼容 Spin？M1 兼容，M3 原生 SDK。

---

## Changelog

- v2.13：新增 §11.3——与 Spin `HandlerType` / `trigger-http` 执行路径对照纪要（Beenet 固定 P2 incoming-handler、双 `instantiate` 语义、`_instance` 保活、`get_instance_pre` 非重复加载）。
- v2.12：M1 档 0 Wasip2 端到端跑通（`curl POST /run/ipfs/<cid>` 返回 200 + 正确 body）。worker 切 `wasmtime-wasi-http::p2::ProxyPre`，引入 `TaskExecutor` trait + `Wasip2HttpExecutor` 实现，加并发闸门（`available_parallelism * 4`）。删 `crates/beenet-worker/wit/` 三份自写 WIT。D16 表述精确化为 `WasiCtxBuilder` capability-not-granted。
- v2.11：`wasm-tools component wit` 验证 spin-sdk 5.x 的 `#[http_component]` 编译产物原生导出 `wasi:http/incoming-handler@0.2.0`。统一 M1 档 0 = Wasip2，废弃档 0-a/0-b 双档拆分。D9 改写。§10 M1 下列出待修项（worker bindgen 对错了 WIT）。
- v2.10：对齐 M1 实际代码仓实况。修订 D9、D10（pin 延后到 M1.5）、D14（M1 仅读 manifest）。新增 D16（M1 WASI 默认姿态）。§7 区分已落地与规划。§10 拆出 M1.5 里程碑。§11.2 补并发闸门阈值与 stdout/stderr wire 扩展。
- v2.9：补 §3.6 计费与计量，D15。
- v2.8：双层内存控制（D14）+ 文档精简。
- v2.7：FAQ 10 条。
- v2.6：隔离上限细化（CPU/内存/出网/并发/RSS 边界）。
- v2.5：补 `MaxInstanceMemoryHook` 与 Trigger hook 取舍说明。
- v2.4：Worker 生命周期分层。
- v2.1-v2.3：D7/D10/D11/D12/D13 关键拍板。
