# Beenet

基于 CID 的分布式 Agent 任务网络，Spin / Wasmtime 嵌入式宿主。

**设计文档**：见 [`readme.md`](./readme.md)（v2.9 架构规格）。

## M1 初始版本

本仓库当前对应 `readme.md §10` 的 **M1 里程碑初始版本**：

- Gateway 裸 HTTP（`POST /run/ipfs/:cid`）
- Worker 本地文件当"伪 CID"（`./wasm_cache/<cid>.wasm`）
- libp2p request-response 协议（`/beenet/invoke/1.0`）
- 单 Worker、无 LB、无 Registry
- 档 0 执行器：直接用 `wasmtime-wasi-http`，支持 `#[http_component]` 任务

已知差距（M1.5 / M2+ 补齐）：

- 未走 `spin_factors_executor::FactorsExecutor::load_app` 管道（动态 `LockedApp` 构造，D4）
- 未真正集成 `BeenetFactors` 作为 Spin `RuntimeFactors`（结构已留位，但 M1 执行器直接用 wasmtime）
- 未接 IPFS；Loader 是 `LocalFileLoader`
- Registry / LB 未实现（M2）

## 仓库结构

```
beenet/
├── crates/
│   ├── beenet-common/       # CID、配置、错误类型
│   ├── beenet-proto/        # InvokeRequest / InvokeResponse (msgpack)
│   ├── beenet-manifest/     # task manifest TOML + Wasm custom section
│   ├── beenet-pack/         # CLI: build / inspect
│   ├── beenet-worker/       # P2pTrigger + Wasip2HttpAdapter + TaskCache
│   └── beenet-gateway/      # HTTP → libp2p 转发
└── examples/
    └── hello-filter-http/   # M1 档 0 示例任务（spin-sdk）
```

## 快速上手

```sh
# 1. 构建整个 workspace
cargo build --release

# 2. 打包示例任务为 task.wasm + 内嵌 manifest
cd examples/hello-filter-http
cargo build --target wasm32-wasip2 --release
cd ../..
./target/release/beenet-pack build \
    --wasm examples/hello-filter-http/target/wasm32-wasip2/release/hello_filter_http.wasm \
    --manifest examples/hello-filter-http/beenet.toml \
    --out dist/task.wasm

# 3. 启动 Worker
mkdir -p wasm_cache
cp dist/task.wasm wasm_cache/$(./target/release/beenet-pack inspect dist/task.wasm | grep -oE 'Qm[A-Za-z0-9]+' | head -1).wasm
./target/release/beenet-worker &

# 4. 启动 Gateway（连到 Worker）
export BEENET_WORKER_ADDR=/ip4/127.0.0.1/tcp/4001
./target/release/beenet-gateway &

# 5. 调用
curl -X POST http://localhost:8080/run/ipfs/<CID> \
     --data 'some text with badword'
```

## 依赖

- Rust 1.91+
- Wasm guest 需 `wasm32-wasip2` target：`rustup target add wasm32-wasip2`
- M1 初始实现直接使用 `wasmtime` / `wasmtime-wasi` / `libp2p`；`FactorsExecutor` 集成留到 M1.5
