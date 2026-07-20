# Beenet

基于 CID 的分布式 Wasm 任务网络，采用 Spin / Wasmtime 风格的嵌入式宿主架构。

- **CID 即函数地址**：Wasm 二进制哈希就是调用地址。
- **P2P 即调用总线**：Gateway / Agent 通过 libp2p 调 Worker。
- **Wasm 即计算体**：毫秒级实例化、强隔离。

> 完整架构、决策记录与里程碑见 [`target.md`](./target.md)。

## 现状（M1 / M1.5）

最短闭环已端到端跑通：

```text
curl → Gateway(HTTP) → libp2p → Worker → wasi:http/incoming-handler@0.2 → body 回传
```

**必须** 运行 **`beenet-registry`**：新 Worker 使用短期 **join token** 调用 **`POST /v1/workers/join`** 完成首次入网；之后只使用本地持久化的 Ed25519 identity 对 **`POST /v1/workers/heartbeat`** 签名续租，不再依赖 join token（详见 [`target.md` §4.1 / §4.3](./target.md)）。Worker 会把 `supported_cids` 一并上报，Gateway 轮询 **`GET /v1/workers`** 后先做 CID hint 过滤，再选择可拨号列表。

**Wasm 分发（推荐）**：`beenet-pack build` 后使用 **`beenet-pack upload`** 推到 **阿里云 OSS**（S3 兼容 API）；在 **`config.toml` 的 `[worker]`** 中配置 **`wasm_fetch_base`**，在 **`wasm_cache` 未命中** 时 **`GET {base}/{cid}`** 拉取并 **校验 CID** 后缓存（见 [`target.md` §3.1](./target.md)）。

档 0 接口走 W3C 标准 `wasi:http/incoming-handler@0.2`。任务作者可以直接用 `spin-sdk 5.x` 的 `#[http_component]` 写业务，Worker 侧通过 `wasmtime-wasi-http` 的 p2 `ProxyPre` 执行。

## 先决条件

- Rust stable（workspace 的 `rust-toolchain.toml` 已固定）
- `wasm32-wasip2` target：`rustup target add wasm32-wasip2`

## 配置文件

**Gateway / Worker / `beenet-pack upload`** 从 **TOML** 读运行参数（日志仍可用 `RUST_LOG`）。**`beenet-registry`** 不读配置文件，只用 CLI（`--http-addr`、`--redis-url`）。

- **默认路径**：`dirs::config_dir()/beenet/config.toml`
- **覆盖**：**`--config /path/to/config.toml`**（Gateway / Worker 支持）
- **Gateway 容器模式**：无配置文件时，只要传入 **`--registry-url`**（及可选 **`--http-addr`**）即可启动

`beenet-pack build` / **`inspect`** **不需要** 配置文件。

开发与联调可共用一份配置，例如：

```toml
[gateway]
http_addr = "127.0.0.1:8080"
registry_url = "http://127.0.0.1:3030"

[worker]
listen_addr = "/ip4/127.0.0.1/tcp/4001"
registry_url = "http://127.0.0.1:3030"
wasm_cache_dir = "wasm_cache"
# wasm_fetch_base = "https://my-bucket.oss-cn-hangzhou.aliyuncs.com/beenet"

[oss]
endpoint = "https://oss-cn-hangzhou.aliyuncs.com"
bucket = "my-bucket"
access_key_id = "LTAI..."
access_key_secret = "..."
region = "oss-cn-hangzhou"
```

- **`[oss]`**：`beenet-pack upload` 必填；也可用 CLI **`--oss-endpoint`** 等覆盖。
- **`wasm_fetch_base`**：不要末尾 `/`；实际请求 **`{base}/{cid}`**。

也可直接使用 **`examples/local-dev-config.toml`**：已预置 **MinIO**（本地 S3）的 `[oss]` 与 `[worker].wasm_fetch_base`，配合 `docker/docker-compose.dev.yml` 做 upload 联调。

## 容器化（Registry + Gateway + Dashboard + MinIO）

`registry`、`gateway` 与 `dashboard` 提供普通 Dockerfile；**worker 仍在宿主机运行**（依赖 Spin 宿主，暂不容器化）。

`docker/docker-compose.dev.yml` 会一并启动：

| 服务 | 宿主机端口 | 说明 |
| --- | --- | --- |
| Redis | 6379 | Registry 持久化 Worker 注册信息 |
| MinIO (S3 API) | 9000 | `beenet-pack upload` 目标 |
| MinIO Console | 9001 | Web 控制台（`minioadmin` / `minioadmin`） |
| beenet-registry | 3030 | 控制面（join / heartbeat / worker 列表） |
| beenet-gateway | **18080** / **14001** | HTTP 入口 + libp2p（Worker 主动 dial 保持反向长连接） |
| beenet-dashboard | 8081 | Registry 管理控制台，使用 admin token 登录 |

```bash
# 构建镜像（在仓库根目录）
make docker-build
# 若需要显式指定代理（本机代理端口 7890）：
#   HTTP_PROXY=http://host.docker.internal:7890 HTTPS_PROXY=http://host.docker.internal:7890 make docker-build

# 启动 Redis + MinIO + registry + gateway
docker compose -f docker/docker-compose.dev.yml up -d --no-build
docker compose -f docker/docker-compose.dev.yml logs -f beenet-registry
```

Dashboard 提供两套构建文件：

```bash
# 普通多阶段构建，自动执行 npm ci 与 npm run build
docker build -f docker/Dockerfile.dashboard \
  --build-arg HTTP_PROXY=http://host.docker.internal:7890 \
  --build-arg HTTPS_PROXY=http://host.docker.internal:7890 \
  -t beenet/dashboard:dev .

# 阿里云 ACR/ECI：Node builder + ACR amd64 runtime 多阶段构建
docker build --pull=false --platform linux/amd64 \
  --build-arg HTTP_PROXY=http://host.docker.internal:7890 \
  --build-arg HTTPS_PROXY=http://host.docker.internal:7890 \
  -f docker/Dockerfile.dashboard.acr \
  -t teethlink-global-registry.cn-hongkong.cr.aliyuncs.com/beenet/dashboard:latest .
```

两套 Dockerfile 都会在 builder 阶段执行 `npm ci` 与 `npm run build`，宿主机不需要安装 Node/npm。`Dockerfile.dashboard.acr` 使用 Node builder 与 ACR 中的 amd64 Debian runtime，适用于阿里云 ECI；普通 `Dockerfile.dashboard` 使用 Docker Hub 的 Node/nginx 多架构镜像，供本地和通用 CI 构建。

上述 build args 用于镜像内部的 `npm`/`apt` 请求。如果失败发生在 `load metadata` 或 `fetch oauth token`，说明基础镜像拉取没有经过代理，需要在 Docker Desktop 的代理设置中将 HTTP/HTTPS proxy 指向宿主机 `127.0.0.1:7890` 后重启 Docker Desktop；Dockerfile build args 无法影响 daemon 拉取 `FROM` 镜像。

MinIO 使用 `quay.io/minio/*` 镜像（避免 Docker Hub 拉取超时）。`minio-init` 会自动创建 bucket `beenet` 并开启匿名下载，Worker 可通过 `http://127.0.0.1:9000/beenet/<cid>` 拉取 wasm。

**upload 到 MinIO**（需先 `cargo build --release -p beenet-pack` 并完成 §2 打包）：

```bash
./target/release/beenet-pack upload \
  --config examples/local-dev-config.toml \
  --wasm dist/task.wasm
```

`examples/local-dev-config.toml` 中相关片段：

```toml
[worker]
wasm_fetch_base = "http://127.0.0.1:9000/beenet"

[oss]
endpoint = "http://127.0.0.1:9000"
bucket = "beenet"
access_key_id = "minioadmin"
access_key_secret = "minioadmin"
region = "us-east-1"
force_path_style = true   # MinIO 必须
```

本地 compose 里我把 registry 的 admin token 固定成了：

```text
beenet-dev-admin-token
```

用它签发 Worker 入网用的 **join token**：

```bash
ADMIN_TOKEN="beenet-dev-admin-token"
curl -s -X POST http://127.0.0.1:3030/v1/admin/tokens \
  -H "Authorization: Bearer $ADMIN_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"description":"dev","ttl_secs":600}'
# 响应 JSON 里的 token_value 即 join_token
```

Join token 默认有效期为 10 分钟，管理员可设置的最大有效期为 60 分钟；有效期内同一个 token 可用于批量接入多个不同 PeerId。Registry 只保存 token 的 SHA-256 摘要，管理列表不会再次返回明文。

**Gateway 在 Docker 里、Worker 在宿主机时**：Worker 只需要 `registry_url` 和 `join-token`，会先向 Registry 领取 Worker 租约，再由 Registry 返回可用 Gateway 列表；Worker 自己不会再手工指定 `gateway_addr`：

```bash
./target/release/beenet-worker \
  --config examples/local-dev-config.toml \
  --join-token-file /path/to/temporary-join-token
```

也可以用 `--join-token-stdin` 交互输入或通过管道传入。`--join-token` 仍兼容，但可能把 token 暴露在 shell history 或进程列表中；`[worker].join_token` 仅保留旧配置兼容，新部署不要持久化它。首次 join 成功后可以立即删除临时 token 文件。Worker 重启时会复用 `wasm_cache_dir/identity.key`，直接签名 heartbeat；若管理员撤销该 registration，则必须显式提供新的有效 join token 才能重新入网。

Dashboard 也同样只读 Registry 的 `/v1/dashboard/status`，不再依赖 Gateway 的状态接口；Worker 的在线状态由 Registry 快照里的 `connected` 字段直接呈现。

然后通过 Docker Gateway 发起请求（注意端口 **18080**）：

```bash
export CID="$(./target/release/beenet-pack inspect dist/task.wasm | awk '/^CID:/{print $2}')"
curl -i -X POST "http://127.0.0.1:18080/run/ipfs/$CID" \
  --data 'please route this support ticket to billing'
```

Worker 在缓存未命中时会从 MinIO 拉取 wasm（日志可见 `fetching wasm into cache`）。

Gateway 与 Worker 都在宿主机时，可继续用 `127.0.0.1:8080`（见下文 §4–§7）。

## 端到端示例

以下命令假定当前目录为 **仓库根目录**（与 `Cargo.toml` 同级）。已在本机用 **`cargo build` 产物 + `examples/local-dev-config.toml` + 本地 `wasm_cache`** 跑通：**`GET /v1/workers` 可见 Worker，`curl` 返回 200**。

**配置文件**：可将上文「配置文件」中的 TOML 存到默认路径；或直接使用仓库里的 **`examples/local-dev-config.toml`**（仅本地缓存、无 `[oss]`），启动时加 **`--config examples/local-dev-config.toml`**。

### 1. 编译宿主与示例任务

```bash
# Host 二进制（registry / gateway / worker / pack）
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
CID: …（以你本机 `beenet-pack inspect dist/task.wasm` 的输出为准，不同构建可能不同）
OUT: dist/task.wasm
SIZE: …
```

**配置文件**：可将上文「配置文件」中的 TOML 存到默认路径；或直接使用 **`examples/local-dev-config.toml`**（含 MinIO `[oss]`，也可删掉 `[oss]` / `wasm_fetch_base` 走 §3b 本地缓存）。

### 3. 发布 wasm（S3 兼容存储）

#### 3a. 本地 MinIO（推荐用于开发）

先 `docker compose -f docker/docker-compose.dev.yml up -d` 启动 MinIO，再 upload：

```bash
./target/release/beenet-pack upload \
  --config examples/local-dev-config.toml \
  --wasm dist/task.wasm
```

验证对象可读：

```bash
CID=$(./target/release/beenet-pack inspect dist/task.wasm | awk '/^CID:/{print $2}')
curl -s -o /dev/null -w "%{http_code}\n" "http://127.0.0.1:9000/beenet/${CID}"
# 预期 200
```

#### 3b. 阿里云 OSS（生产）

在 **`config.toml`** 的 **`[oss]`** 中填入 **RAM 子账号** 的 AK/SK（勿提交仓库）。典型 **华东1（杭州）**：[oss] 示例见「配置文件」一节。然后：

```bash
./target/release/beenet-pack upload --config /path/to/config.toml --wasm dist/task.wasm
```

也可用 **`--oss-endpoint`、`--oss-bucket`、…** 覆盖文件中的单个字段。

若 PutObject 失败，可在 **`[oss]`** 中设 **`force_path_style = true`** 再试。

#### 3c. 仅本地缓存（离线调试）

不写 S3 时，可把打包产物拷入缓存：

```bash
CID=$(./target/release/beenet-pack inspect dist/task.wasm | awk '/^CID:/{print $2}')
cp dist/task.wasm "wasm_cache/${CID}.wasm"
```

### 4. 启动 Registry（控制面）

Registry 依赖 **Redis**（默认 `redis://127.0.0.1:6379`）。本地可先起一个 Redis：

```bash
docker run -d --name beenet-redis -p 6379:6379 redis:7-alpine
```

然后启动 registry（**无需** `--config`）：

```bash
RUST_LOG=info ./target/release/beenet-registry --http-addr 127.0.0.1:3030
```

启动日志会打印 **Admin Token**。用它创建 Worker 用的 **join token**（见上文「容器化」一节里的 `curl` 示例），记下 `token_value`。

### 5. 启动 Worker

另开终端（`cd` 到仓库根目录，保证 `wasm_cache` 相对路径一致）：

```bash
RUST_LOG=info ./target/release/beenet-worker \
  --config examples/local-dev-config.toml \
  --join-token-file /path/to/temporary-join-token
```

确认入网：

```bash
curl -s http://127.0.0.1:3030/v1/workers | jq .
```

### 6. 启动 Gateway

```bash
RUST_LOG=info ./target/release/beenet-gateway --config examples/local-dev-config.toml
```

若 Gateway 跑在 Docker 里，改用 `--registry-url http://127.0.0.1:3030`（无需配置文件），并把对外端口改为 compose 映射的 **18080**（见 `docker/docker-compose.dev.yml`）。

### 7. 发起请求

```bash
export CID="$(./target/release/beenet-pack inspect dist/task.wasm | awk '/^CID:/{print $2}')"
curl -i -X POST "http://127.0.0.1:8080/run/ipfs/$CID" \
  --data 'please route this support ticket to billing'
```

预期：

```text
HTTP/1.1 200 OK
x-beenet-status: ok
content-length: 27

{"label":"billing","action":"route to finance","summary":"please route this support ticket to billing"}
```

若 **`connection refused`**：确认三进程已启动且端口未被占用。若 **`x-beenet-status: load-error`**：确认 wasm 已 upload 到 S3 / 拷入 `wasm_cache`，且 Worker 的工作目录与 `wasm_cache_dir` 一致。若 **`x-beenet-status: runtime-error`** 且 Gateway 在 Docker 内：检查 Worker 是否仍在运行，以及 `listen_addr` 是否为宿主机可达 IP（非 `127.0.0.1`）。

## 写你自己的任务

1. 新建一个 `wasm32-wasip2` 的 `cdylib` crate，依赖 `spin-sdk`，用 `#[http_component]` 导出 handler。
2. 写一份 `beenet.toml`（字段见 [`crates/beenet-manifest`](./crates/beenet-manifest) 和示例 [`examples/hello-filter-http/beenet.toml`](./examples/hello-filter-http/beenet.toml)），`interface` 固定为 `wasi:http/incoming-handler@0.2`。
3. 用 `beenet-pack build` 产出 `task.wasm`，**`upload` 到 OSS** 或拷入 `wasm_cache`，拿到 CID 后即可通过 Gateway 调用。

## 仓库结构

| 路径 | 说明 |
| --- | --- |
| `crates/beenet-common` | `BeenetCid`（CIDv1 / raw / sha2-256）+ 协议常量 |
| `crates/beenet-proto` | `InvokeRequest` / `InvokeResponse` / `Status` / `Usage` |
| `crates/beenet-manifest` | `beenet:manifest/v1` TOML schema + wasm custom section |
| `crates/beenet-pack` | `build` / `inspect` / **`upload`**（S3 兼容，OSS） |
| `crates/beenet-registry` | HTTP Registry：Worker **心跳** `POST /v1/workers/heartbeat`、`GET /v1/workers` |
| `crates/beenet-worker` | libp2p + Factors；Registry；**可选 HTTP 拉取 wasm** |
| `crates/beenet-gateway` | HTTP → libp2p；**必填** Registry URL，轮询 Worker 列表 |
| `examples/hello-filter-http` | `#[http_component]` 工单分类示例任务 |
| `examples/local-dev-config.toml` | 本地联调配置（MinIO `[oss]` + gateway / worker） |
| `docker/` | Registry / Gateway / Dashboard Dockerfile、`docker-compose.dev.yml`（含 MinIO） |
| `Makefile` | `make build`、`make docker-build`、`make docker-up` 等 |

## 许可

见 [`LICENSE`](./LICENSE)。
