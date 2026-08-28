# deploy

Beenet 的交付物都在这里。不要再拆成 `apps/`、`docker/`、`beenet-deploy/`。

| 路径 | 给谁用 | 做什么 |
| --- | --- | --- |
| [`macos-contributor/`](macos-contributor/) | 贡献者 Mac | Swift App + `build.sh`，产出 `dist/Beenet.app` / DMG。`guest/alpine-init` 是 vfkit 里 Alpine 客户机的 PID 1 |
| [`linux/`](linux/) | 贡献者 Linux | systemd unit（`Delegate=yes`）。安装脚本仍在仓库根 `scripts/get-bworker.sh` |
| [`windows/`](windows/) | 贡献者 Windows | 本机进程 + Job Objects。Inno Setup 向导产出 `BeenetSetup-x64.exe`；应用里改名称和地区 |
| [`docker/`](docker/) | 构建镜像 | Registry / Gateway / Front Door / guest VM / Linux worker 的 Dockerfile |
| [`docker-compose.dev.yml`](docker-compose.dev.yml) | 本地开发 | Redis + Registry + Gateway。用 `./scripts/dev-up.sh` |
| [`kubernetes/`](kubernetes/) | 生产集群 | 纯 YAML 与 Helm charts |

```bash
make dmg                          # macOS 贡献者 App
make linux-worker-tarball         # Linux worker + unit
make windows-installer            # Windows 安装程序（需 Inno Setup）
make docker-up                    # 本地 compose
make deploy                       # kubectl apply kubernetes/
```
