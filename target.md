# Beenet —— 基于 CID 的分布式 Agent 任务网络（恢复版）

> 文档版本：**v2.9（恢复版）**  
> 状态：架构设计（pre-MVP）  
> 说明：本文件根据会话记录重建，覆盖此前 1500+ 版本的核心结构与决策（D1-D15）。

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
- stdout/stderr 与资源使用量结构化回传。

### 1.2 非功能目标

- 冷启动 P95 < 100ms（缓存命中更低）。
- 并发目标：单 Worker `CPU * 4` 级别。
- Gateway 无状态，可水平扩展。
- 默认安全姿态：不信任任意 CID，出网默认拒绝，文件默认不挂载。

### 1.3 项目形态与依赖策略

- **D1**：独立仓库 + Host Embedding（不 fork Spin 主仓）。
- **D2**：Spin 核心依赖以 `git rev` 锁定（本地可 `[patch]` 覆盖为 path）。
- **D10**：初始 pin 到 `6d9e8c79...`（会话中已拍板）。

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

- **档 0（M1）**：兼容 `#[http_component]`，通过合成 HTTP Request 执行 `fermyon:spin/inbound-http`。
- **档 1（M3）**：`beenet:task/runner@0.1` 原生接口。

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

## 7. 仓库骨架（规划）

- `crates/beenet-worker`
- `crates/beenet-gateway`
- `crates/beenet-registry`（M2）
- `crates/beenet-proto`
- `crates/beenet-manifest`
- `crates/beenet-pack`
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

- **M1**：最短闭环（单 worker，本地伪 CID，档 0）
- **M2**：多 worker + Registry + LB
- **M2.5**：WIT 草案
- **M3**：IPFS + 原生 SDK（档 1）
- **M4**：生产化（鉴权、计费、可观测、流式）

---

## 11. 决策记录（恢复）

### 11.1 已决策（D1-D15）

- D1 项目形态：独立仓库 Host Embedding
- D2 依赖策略：git rev 统一 pin
- D3 Factor 组装：扁平 `BeenetFactors`
- D4 M1 加载路径：动态 LockedApp（后续迁 core 直连）
- D5 文件系统默认姿态：不挂载
- D6 出网默认姿态：默认拒绝
- D7 任务接口路线：档 0 + 档 1 共存
- D8 缓存策略：per-CID + LRU + singleflight
- D9 M1 执行器：Wasip2HttpAdapter
- D10 Spin commit 初始 pin
- D11 返回值语义：A/B 表映射
- D12 打包格式：单文件 + custom section
- D13 manifest 来源优先级：section 主、local policy 兜底
- D14 内存上限双层：L1 Hook + L2 handler
- D15 计费模型：三层账单 + 边界规则

### 11.2 待定（恢复）

- 幂等默认策略
- CID 白名单/签名策略
- Registry 一致性方案
- Worker 策略阈值实测
- 计费单价参数（运营）
- Fuel 档性能实测与 dry-run API 形态

---

## 12. 参考锚点（恢复）

- `spin/crates/trigger/src/lib.rs`
- `spin/crates/trigger-http/src/wasi.rs`
- `spin/crates/factors-executor/src/lib.rs`
- `spin/crates/core/src/store.rs`
- `spin/crates/core/src/limits.rs`

---

## 13. FAQ（恢复）

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

## Changelog（恢复摘要）

- v2.9：补 §3.6 计费与计量，D15。
- v2.8：双层内存控制（D14）+ 文档精简。
- v2.7：FAQ 10 条。
- v2.6：隔离上限细化（CPU/内存/出网/并发/RSS 边界）。
- v2.5：补 `MaxInstanceMemoryHook` 与 Trigger hook 取舍说明。
- v2.4：Worker 生命周期分层。
- v2.1-v2.3：D7/D10/D11/D12/D13 关键拍板。
