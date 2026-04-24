# Beenet

基于 CID 的分布式 Wasm 任务网络，采用 Spin / Wasmtime 风格的嵌入式宿主架构。

- **CID 即函数地址**：Wasm 二进制哈希就是调用地址。
- **P2P 即调用总线**：Gateway / Agent 通过 libp2p 调 Worker。
- **Wasm 即计算体**：毫秒级实例化、强隔离。

> 完整架构、决策记录与里程碑见 [`target.md`](./target.md)。

## 现状（M1）

最短闭环已端到端跑通：

```text
curl → Gateway(HTTP) → libp2p → Worker → wasi:http/incoming-handler@0.2 → body 回传
```

档 0 接口走 W3C 标准 `wasi:http/incoming-handler@0.2`。任务作者可以直接用 `spin-sdk 5.x` 的 `#[http_component]` 写业务，Worker 侧通过 `wasmtime-wasi-http` 的 p2 `ProxyPre` 执行。

## 先决条件

- Rust stable（workspace 的 `rust-toolchain.toml` 已固定）
- `wasm32-wasip2` target：`rustup target add wasm32-wasip2`

## 端到端示例

以 `examples/hello-filter-http`（把请求体里的 `badword` 替换成 `***`）为例：

### 1. 编译宿主与示例任务

```bash
# Host 二进制（gateway / worker / pack）
cargo build --release --workspace

# 示例任务组件（wasm32-wasip2）
cargo build --release \
  --manifest-path examples/hello-filter-http/Cargo.toml \
  --target wasm32-wasip2
```

### 2. 打包成带 manifest 的单文件 wasm

```bash
mkdir -p dist wasm_cache

./target/release/beenet-pack build \
  --wasm examples/hello-filter-http/target/wasm32-wasip2/release/hello_filter_http.wasm \
  --manifest examples/hello-filter-http/beenet.toml \
  --out dist/task.wasm
```

输出形如：

```text
CID: bafkreictt2soyhv22jgeelsym7uecfqbr6wyme5xtsaekwabxm5ztvh2ge
OUT: dist/task.wasm
SIZE: 262993
```

### 3. 灌入 Worker 本地缓存

M1 的 loader 从 `./wasm_cache/<cid>.wasm` 读组件（`target.md §3.1`）：

```bash
CID=$(./target/release/beenet-pack build \
  --wasm examples/hello-filter-http/target/wasm32-wasip2/release/hello_filter_http.wasm \
  --manifest examples/hello-filter-http/beenet.toml \
  --out dist/task.wasm | awk '/^CID:/{print $2}')

cp dist/task.wasm wasm_cache/$CID.wasm
echo "CID=$CID"
```

### 4. 启动 Worker

```bash
RUST_LOG=info ./target/release/beenet-worker \
  --listen-addr /ip4/127.0.0.1/tcp/4001
```

在日志里记下 `worker reachable at /ip4/127.0.0.1/tcp/4001/p2p/<peer-id>`。

### 5. 启动 Gateway

用 `BEENET_WORKER_ADDR` 告诉 Gateway 上一步的 Worker multiaddr（M1 硬编码单 Worker，Registry / LB 留到 M2）：

```bash
BEENET_WORKER_ADDR=/ip4/127.0.0.1/tcp/4001/p2p/<peer-id> \
RUST_LOG=info ./target/release/beenet-gateway
```

Gateway 默认监听 `127.0.0.1:8080`。

### 6. 发起请求

```bash
curl -i -X POST http://127.0.0.1:8080/run/ipfs/$CID \
  --data 'please filter my badword please'
```

预期：

```text
HTTP/1.1 200 OK
x-beenet-status: ok
content-length: 27

please filter my *** please
```

## 写你自己的任务

1. 新建一个 `wasm32-wasip2` 的 `cdylib` crate，依赖 `spin-sdk`，用 `#[http_component]` 导出 handler。
2. 写一份 `beenet.toml`（字段见 [`crates/beenet-manifest`](./crates/beenet-manifest) 和示例 [`examples/hello-filter-http/beenet.toml`](./examples/hello-filter-http/beenet.toml)），`interface` 固定为 `wasi:http/incoming-handler@0.2`。
3. 用 `beenet-pack build` 产出 `task.wasm`，拿到 CID 后即可通过 Gateway 调用。

## 仓库结构

| 路径 | 说明 |
| --- | --- |
| `crates/beenet-common` | `BeenetCid`（CIDv1 / raw / sha2-256）+ 协议常量 |
| `crates/beenet-proto` | `InvokeRequest` / `InvokeResponse` / `Status` / `Usage` |
| `crates/beenet-manifest` | `beenet:manifest/v1` TOML schema + wasm custom section |
| `crates/beenet-pack` | `build` / `inspect` CLI |
| `crates/beenet-worker` | libp2p 请求入口 + `TaskExecutor` 抽象 + `Wasip2HttpExecutor` |
| `crates/beenet-gateway` | HTTP `POST /run/ipfs/:cid` → libp2p 转发 |
| `examples/hello-filter-http` | `#[http_component]` 示例任务 |

## 许可

见 [`LICENSE`](./LICENSE)。
