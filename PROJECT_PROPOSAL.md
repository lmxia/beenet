# Beenet 项目立项申请书

> 基于 CID 的去中心化 Wasm 边缘任务网络

---

## 一、立项背景：行业格局的历史性转折

### 1.1 OpenAI 收购 Cloudflare：AI 大脑与神经末梢的融合

2026 年，OpenAI 寻求收购 Cloudflare 的战略意图已公开。这一动作背后的逻辑清晰而深刻：大模型作为 AI 的"大脑"，其推理能力的价值需要通过遍布全球的"神经末梢"——即边缘网络节点——才能以最低延迟、最高可用性触达用户。OpenAI 的战略目标是构建**大模型 + 边缘网络的一体化平台**，将推理能力下沉至距用户最近的节点，消除云中心化带来的延迟瓶颈。

这一收购若落地，意味着 AI 计算范式的根本性转变：
- **从中心化推理 → 边缘分布式推理**
- **从通用云平台 → AI 原生边缘网络**
- **从 API 调用 → 就地执行**

### 1.2 Akamai 收购 Fermyon：Wasm 成为边缘 AI 推理的标准载体

同年年初，Akamai——Cloudflare 最直接的竞争对手——宣布收购 Fermyon。Fermyon 是 **Spin 框架**的创造者，Spin 正是在 WebAssembly（Wasm）生态中最具影响力的边缘计算运行时之一。Akamai 将 Fermyon 的 Wasm 技术引入其核心云平台，目标直指**边缘 AI 推理加速**。

Wasm 为何能成为边缘 AI 推理的标准载体？

| 特性 | 价值 |
|------|------|
| **毫秒级冷启动** | 边缘节点无需预热，按需实例化 |
| **强沙箱隔离** | 多租户模型安全并发执行 |
| **跨平台可移植** | 相同二进制运行在 x86、ARM、RISC-V 边缘硬件 |
| **确定性执行** | Fuel 计量使算力可计费、可审计 |
| **WASI 标准接口** | 与宿主环境解耦，能力按需授权 |

### 1.3 行业共识：边缘 AI 推理的基础设施窗口期

两大巨头的并购行动，传递出同一个信号：**边缘 AI 推理基础设施的建设窗口已经开启**。谁能建立开放、可扩展的边缘 Wasm 执行网络，谁就掌握了下一代 AI 应用的分发管道。

---

## 二、现有方案的局限与市场空白

### 2.1 Cloudflare Workers / Akamai+Spin：平台封闭，算力垄断

现有商业边缘计算平台均以**自有网络节点**为算力来源，用户只能接受平台提供的节点、定价与治理规则：

- **算力来源单一**：无法利用企业内网机器、个人边缘设备、异构云节点
- **CID 寻址缺失**：代码版本与执行节点绑定，无法实现内容可寻址的透明执行
- **P2P 能力缺失**：节点间调用必须经由平台中心化调度，不支持直连 Agent 架构
- **计量不透明**：算力消耗由平台单方面计量，缺乏可验证的执行证明

### 2.2 市场空白：开放的去中心化 Wasm 边缘执行网络

目前没有任何开源项目或商业产品能同时满足：

> **内容寻址（CID）+ P2P 调用总线（libp2p）+ Wasm 沙箱执行（Wasmtime）+ 开放节点接入（任意算力加入）**

这正是 **Beenet 的立项机会**。

---

## 三、Beenet 的技术定位与核心设计

### 3.1 核心命题：寻址即计算

Beenet 提出一个根本性的计算范式：**CID 即函数地址**。

```
任务代码（Wasm 二进制）的内容哈希（CIDv1/raw/sha2-256）= 该任务的全局唯一调用地址
```

这意味着：
- 调用方只需持有 CID，无需知道任务运行在哪个节点
- Worker 节点通过内容校验自证执行的是正确版本
- 任务版本演进天然形成内容可寻址的版本历史

### 3.2 架构三支柱

**① CID 即函数地址**

`beenet-pack build` 将 Wasm 二进制与 `beenet.toml` 清单（运行时限制、能力声明、计量策略）打包为单文件，计算 CIDv1/raw/sha2-256 作为全局调用地址。CID 既是寻址凭据，也是完整性证明——Worker 拉取后必须校验哈希，篡改即拒绝执行。

**② P2P 即调用总线**

Gateway 与 Agent 通过 **libp2p**（协议 `/beenet/invoke/1.0`）向 Worker 发起调用。这使得：
- Worker 可部署在内网、家庭宽带、企业私有云等任意位置，无需公网直接暴露
- NAT 穿透与 Relay 机制使异构网络拓扑下的算力得以激活
- Agent 可绕过 Gateway 直接 P2P 调用 Worker，实现零中心化的 Agent-to-Worker 协作

**③ Wasm 即计算体**

Beenet 采用 Spin/Wasmtime 风格的嵌入式宿主架构（Host Embedding），不依赖 `spin up` CLI 壳，直接嵌入 Wasmtime 运行时，实现：
- 冷启动 P95 < 100ms（预编译 + InstancePre 缓存）
- 强沙箱隔离（每请求独立 Store，进程级 cgroup 兜底）
- 能力最小化授权（`BeenetFactors` 扁平结构：WasiFactor / OutboundNetworkingFactor / AuditFactor）
- 默认拒绝出网、默认不挂载文件系统

### 3.3 与 Spin 生态的关系：兼容而超越

Beenet **完全兼容 Spin SDK**。任务作者可直接使用 `spin-sdk 5.x` 的 `#[http_component]` 宏编写业务逻辑，编译为 `wasm32-wasip2` 目标后，无需任何修改即可在 Beenet Worker 上执行（档 0：`wasi:http/incoming-handler@0.2`）。

这意味着：
- **Fermyon/Spin 生态的所有任务开发者，都是 Beenet 的潜在用户**
- Beenet 不是 Spin 的竞争者，而是在 Spin 执行层之上构建的**去中心化调度与分发网络**
- Akamai 收购 Fermyon 加速了 Spin 任务的生态积累，Beenet 恰好是这个生态的开放分发层

### 3.4 四层能力边界（Factor 体系）

```
调用方 CID 请求
    ↓
Gateway（HTTP / libp2p）
    ↓ InvokeRequest（CBOR）
Worker（libp2p）
    ↓
BeenetFactors 能力层
  ├── WasiFactor（stdio，无文件系统，无网络）
  ├── VariablesFactor（任务变量注入）
  ├── OutboundNetworkingFactor（出网白名单，manifest 声明）
  ├── OutboundHttpFactor（HTTP 出站，capability-gated）
  └── AuditFactor（wall_ns / cpu_ns / fuel / mem_bytes，结构化计量）
    ↓
Wasm 执行（Wasmtime + wasi:http P2）
    ↓ InvokeResponse（status + body + usage）
```

### 3.5 计量与计费：可验证的算力经济

Beenet 的计量模型基于 Wasmtime 的四个维度：

| 维度 | 用途 |
|------|------|
| `fuel`（指令级） | 确定性算力计费，防止滥用 |
| `cpu_ns`（真实 CPU） | 反映实际资源消耗 |
| `wall_ns`（挂钟时间） | 超时控制与 SLA 计量 |
| `mem_bytes`（线性内存） | 内存占用计费 |

三层账单模型：`bill = base_fee + compute_fee + resource_fee`，为构建开放算力市场提供可信的计量基础。

---

## 四、工作的战略意义

### 4.1 在 OpenAI-Cloudflare 整合浪潮中的位置

OpenAI 收购 Cloudflare，本质上是在建立一个**封闭的 AI 推理帝国**：大模型 + 边缘网络 + 计费体系，全部由单一公司控制。

Beenet 代表了另一条路径：**开放的、去中心化的边缘 AI 推理基础设施**。

```
OpenAI + Cloudflare 路径：
大模型 → 专有边缘网络 → 封闭 API → 用户

Beenet 路径：
任意模型/任务（CID 寻址）→ 开放 P2P 算力网络 → 标准 WASI 接口 → 用户
```

这不是技术选择，而是**基础设施主权**的选择。

### 4.2 在 Akamai-Fermyon 并购中的战略卡位

Akamai 收购 Fermyon 的核心逻辑是：将 Spin 的 Wasm 执行能力嵌入 Akamai 的全球边缘节点，构建**中心化管控的 Wasm 边缘网络**。

Beenet 的卡位在于：**Spin 任务的开放分发层与去中心化执行网络**。

- Akamai 能给你的是：自有节点，按 Akamai 定价，在 Akamai 的治理规则下运行
- Beenet 给你的是：**任意节点加入，CID 内容寻址，P2P 调用，可验证执行，开放计量**

当 Spin 生态的任务数量因 Akamai 的推广而爆发增长时，Beenet 作为这些任务的**替代分发与执行网络**，战略价值随之放大。

### 4.3 激活沉睡算力：内网与边缘节点的价值重估

当前绝大多数企业内网算力（开发机、测试服务器、内网 GPU 节点）无法直接服务外部调用，大量算力处于"沉睡"状态。

Beenet 通过 libp2p 的 NAT 穿透与 Relay 体系，将这些沉睡算力接入全局可寻址的执行网络：

```
企业内网 Worker → 注册到 Registry → Gateway 发现 → 接受 CID 调用 → 执行 → 计量回传
```

这是边缘计算领域尚未被充分开发的资产：**存量算力的网络化激活**。

### 4.4 AI Agent 的去中心化执行底座

随着 AI Agent 从单一大模型调用演进为**多步骤、多工具、多节点的协作网络**，Agent 需要能够直接 P2P 调用分布在网络各处的工具函数（Wasm 任务）。

Beenet 的 P2P 原生入口（Agent 直连 Worker，绕过 Gateway）恰好为这一场景提供了技术基础：

```
AI Agent
  ├── 持有 CID（工具函数的唯一标识）
  ├── 通过 libp2p 发现持有该 CID 的 Worker
  └── 直接 P2P 调用，毫秒级响应，结果可验证
```

---

## 五、项目阶段规划

### 当前状态（M1 已完成）

最短闭环已端到端验证：

```
curl → Gateway(HTTP) → libp2p → Worker → wasi:http/incoming-handler@0.2 → body 回传
```

已落地组件：
- `beenet-common`（CIDv1 内容寻址）
- `beenet-proto`（InvokeRequest/Response/Status/Usage，CBOR）
- `beenet-manifest`（`beenet:manifest/v1` + Wasm custom section）
- `beenet-pack`（build/inspect/upload，S3 兼容，阿里云 OSS）
- `beenet-registry`（HTTP 控制面，心跳/续租/Worker 列表）
- `beenet-worker`（libp2p invoke + Wasmtime 执行 + HTTP 拉取 Wasm）
- `beenet-gateway`（HTTP → libp2p，轮询 Registry 动态调度）

### M1.5（近期）：生产级隔离与能力层

- 接入 `BeenetFactors`（扁平 RuntimeFactors，出网 linker 级钳制）
- 接入 `MaxInstanceMemoryHook` + `StoreLimits`（内存双层控制落地）
- `InvokeResponse` 扩展 `stdout`/`stderr` 回传
- pin 落实 Spin commit（首次引入 `spin-*` 依赖）

### M2（中期）：生产级选路与 DHT

- 按 CID 索引副本，一致性哈希 + least-inflight 负载均衡
- DHT 冷路径发现（弱中心兜底）
- Registry 持久化与高可用（多副本一致性）

### M3（长期）：原生 SDK 与 IPFS

- `beenet-sdk`（`#[beenet_task]` 宏，档 1 原生接口）
- IPFS Gateway 接入（CID 语义完整落地，去中心化存储）
- `beenet:task/runner@0.1` WIT 正式冻结

### M4（生产化）

- 鉴权体系（mTLS、API key、准入凭据）
- 计费管道（fuel 实测 + 三层账单 + 单价参数）
- 可观测平台（结构化 usage 聚合、Prometheus metrics）
- 流式执行（streaming InvokeResponse）

---

## 六、核心竞争壁垒

| 维度 | 竞争优势 |
|------|---------|
| **技术深度** | Host Embedding 架构，非 CLI 壳，Wasmtime 深度集成，可定制 Factor 能力层 |
| **协议开放** | libp2p 标准协议，任意网络拓扑接入，不锁定云厂商 |
| **生态兼容** | 完全兼容 Spin SDK，Fermyon/Akamai 生态任务无需修改即可运行 |
| **内容寻址** | CID 语义保证执行代码的唯一性与可验证性，为可信计算提供基础 |
| **算力来源** | 开放节点接入，激活内网/边缘/异构算力，突破单一云厂商的算力垄断 |
| **计量可信** | Wasmtime fuel/cpu/mem 多维计量，为算力市场提供可验证的计费基础 |

---

## 七、申请事项

### 7.1 立项申请

申请正式立项 **Beenet —— 基于 CID 的去中心化 Wasm 边缘任务网络**，定位为：

> **下一代 AI 边缘推理基础设施的开放协议层与执行网络**

在 OpenAI 收购 Cloudflare 与 Akamai 收购 Fermyon 所定义的行业格局中，Beenet 占据**开放 Wasm 边缘执行网络**这一尚未被封闭平台占据的关键位置。

### 7.2 阶段目标

- **近期（M1.5）**：完成生产级隔离层，具备受控环境下的 Beta 接入能力
- **中期（M2）**：完成生产级选路，支持多 Worker 多 CID 的规模化部署
- **长期（M3）**：发布原生 SDK，对接 IPFS，形成完整的去中心化任务网络生态

### 7.3 资源需求

- 持续的研发投入（Rust / Wasm / libp2p / Wasmtime 技术栈）
- 边缘节点测试环境（覆盖 NAT 穿透、多网络拓扑场景）
- 对象存储资源（阿里云 OSS，用于 Wasm 任务分发）
- 生态合作接洽（Spin 任务开发者社区，边缘算力供应方）

---

## 八、结语

当 OpenAI 与 Akamai 各自用数十亿美元的并购，宣告**边缘 AI 推理基础设施的战略价值**时，Beenet 正在用开放协议、内容寻址与 P2P 组网，构建这个价值的去中心化替代方案。

历史上每一次计算基础设施的范式转移——从大型机到 PC，从 PC 到云计算——都曾产生新的开放协议层（TCP/IP、HTTP、S3 API），这些协议层最终比任何封闭平台都更持久、更具价值。

**Beenet 的目标，是成为边缘 AI 推理时代的那一层开放协议。**

---

*文档基于 Beenet v2.19 技术设计，结合 2026 年行业动态撰写。*
*技术详情见 [`target.md`](./target.md) 与 [`readme.md`](./readme.md)。*
