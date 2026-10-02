# bilibili-danmaku

抓 B 站某个直播间的弹幕，挂在桌面角上显示。**当前进度：抓取 + Wayland 浮层都跑通了**
（匿名就能收）。

```bash
cargo static                                        # musl 静态：dump 2.6MB，浮层 3.3MB
./target/x86_64-unknown-linux-musl/release/danmaku-dump 14709735          # 命令行看弹幕
./target/x86_64-unknown-linux-musl/release/danmaku-overlay 14709735 \
    --anchor top-right --output DP-2 --font ~/.local/share/fonts/LXGWNeoXiHei.ttf   # 桌面浮层
```

（用 `./target/release/...` 则是普通的 glibc 动态构建，调试时改代码快。）

## 浮层（layer-shell）

```bash
./target/release/danmaku-overlay <房间号|链接> [选项]

  --width / --height    浮层宽（默认 420）/ 高度上限（默认 560）
  --margin              离屏幕边缘（默认 16）
  --anchor              bottom-right|bottom-left|top-right|…（默认 bottom-right）
  --layer               overlay|top|bottom|background（默认 top，不挡全屏窗口）
  --font-size           字号（默认 20；是 em 尺寸，汉字实际约占九成）
  --opacity             暗色底板的不透明度（默认 0.55，0 = 只有字没底）
  --ttl / --fade        一条弹幕待多久开始淡出（默认 12s）/ 淡出多久（默认 0.6s）
  --max-lines           最多记多少条（默认 200）
  --output <名字>       挂到哪块显示器（默认合成器挑，如 `--output DP-2`）
  --scale <倍数>        设备像素比（默认跟随合成器，分数缩放 1.25/1.3 也认）
  --font <路径>         字体文件，默认 fontconfig 找中文字体
  --emoji-font <路径>   彩色 emoji 字体（默认 fc-match emoji；找不到就跳过 emoji）
  --line-gap <像素>     行间距（默认 4）
  --config <路径>       配置文件（默认 ~/.config/bilibili-danmaku/config.json）
  --no-config           不读配置文件，只用命令行
  --print-config        把当前生效的设置打成 JSON 就退出
```

### 配置文件 + 设置界面

浮层启动时读 `~/.config/bilibili-danmaku/config.json`（不存在就全用命令行默认值），
之后每帧看一眼 mtime，**改完立刻生效**——字号、行距、透明度、停留时间、淡出、
最多几条、缩放、位置、层级、宽高、边距都是热重载。只有这几项要重启浮层：

```
room   房间号（要重连）          cookie   登录状态
output 显示器（要重建 surface）  font / emoji_font   字体要重新 mmap
```

所以 GUI 不需要任何 IPC，写完文件就完事（原子写：临时文件 + rename）。

```bash
danmaku-overlay --print-config > ~/.config/bilibili-danmaku/config.json   # 拿当前设置当模板
```

`gui/` 里是个 GTK4 设置界面（Python + libadwaita）：**字号、位置、停留时间这些都是滑块**，
位置用九宫格点，旁边一块「屏幕示意」显示浮层挂在哪；改哪项写哪项，浮层那边热重载。

```bash
cd gui && ./danmaku-config      # 直接跑
./gui/install.sh                # 装到 ~/.local，生成 systemd 用户单元 + 应用菜单项
```

显示形态（第一版只做弹幕，礼物/醒目留言以后再说）：

- **没有弹幕时整层完全透明**，一个字都不画
- 有弹幕时是一块**固定宽度的方形暗色半透明底板**（不是每条弹幕一个圆角气泡），
  高度刚好裹住当前可见的那几条，字左对齐
- 新弹幕贴底、旧的往上推，是 **~200ms 的滑动过渡**（整摞先下压一行再滑回去），不是瞬移；
- 一条弹幕太长会在面板宽度内**软换行**：续行缩进一格、标点不落行首，底板跟着变高；
  高度是按「视觉行数」而不是「条数」算的
  超过 `--height` 的老弹幕直接不画
- emoji 走彩色字体（Noto Color Emoji）：主字体没有这个字就去 emoji 字体里取位图，
  按字的宽度缩放、跟汉字对齐；两边都没有的字符**直接跳过**，不留豆腐块
- 每条到 `--ttl` 后开始淡出，底板跟着一起淡；全没了就恢复成全透明
- 不显示房间标题/在线人数/状态（连接、断线这些走 stderr）

Ctrl+C 退出。不抢键盘、鼠标点穿（输入区域设成空的，点击直接落到下面的窗口），
纯挂件，不耽误你点底下的按钮。

渲染路径是纯软件、没有 GTK/Qt/GPU：SCTK 开 wlr-layer-shell 面 → wl_shm 共享内存 →
tiny-skia 画底板、ab_glyph 刷字。动画（推弹幕、淡出）用定时器推：有动画时 16ms 一帧
（60fps，CPU 约 1.6%），安定下来就切回「到点才醒」（最多 1 秒一次），没弹幕时一次都不刷。

### 浮层体积

| 配置 | 二进制 | RSS |
|---|---|---|
| Noto Sans CJK（fc-match 默认），glibc 动态 | 3.1 MB | ~19 MB |
| 同上，musl 静态 | 3.3 MB | ~17 MB |
| `--font ~/.local/share/fonts/LXGWNeoXiHei.ttf`（7MB 字体） | 3.3 MB | ~12.5 MB |

RSS 大头是中文字体：字体 mmap 按需读，Noto Sans CJK 那 19MB 里实际摸到约 8.5MB
（换小字体就降到 <1MB）。剩下的是 shm 双缓冲（420×560×4 ≈ 0.9MB/块）、二进制和 libc。

### 浮层踩过的坑

| 坑 | 结论 |
|---|---|
| 1.25/1.3 倍缩放的屏上字糊 | SCTK 0.21 不认 `wp_fractional_scale_v1`，得自己绑它 + `wp_viewporter`；分数缩放下 `wl_surface.buffer_scale` 必须保持 1，逻辑尺寸用 `wp_viewport.set_destination()` 表达 |
| `--output DP-2` 查不到显示器名字 | `wl_output` 的名字要等一轮事件才到，所以先 `roundtrip()` 再建 layer surface |
| 光加 `--width/--height` 字就吃掉 20MB | 字体别 `fs::read` 进内存（19MB 直接进 RSS），用 mmap + `MADV_RANDOM` |
| 浮层挡住下面的按钮 | `keyboard_interactivity=None` 只管键盘；指针要靠 `wl_surface.set_input_region(空 region)` 才穿得过去（用 `zwlr_virtual_pointer` 注入点击实测过） |
| 蓝色昵称在屏幕上变成橙色 | tiny-skia 的 RGBA8888 在内存里是 `[R,G,B,A]`，wl_shm 的 Argb8888 是 `[B,G,R,A]`，字节序相反；渲染完把 R/B 换一下（有测试盯着） |
| SCTK 0.21 里 `delegate_compositor!` 之类都不见了 | 标准接口全由 `delegate_dispatch2!` 一个宏接管；自己引的协议要 `impl Dispatch2<I, State> for ()` |
| `cargo static` 打浮层时 pkg-config 报 cross-compilation | SCTK 默认的 `xkbcommon` 特性要 C 库，浮层又不抢键盘，`default-features = false` 关掉即可 |
| 想用 `wl_surface.frame()` 当动画节拍 | niri 上它是**跟着我们自己的 commit 回的**（1~4ms 就来），不是按 vblank，等于没有节拍；动画帧得自己用定时器推，frame 回调只能当锦上添花 |

## 命令行 dump

```bash
# 房间号或直播间链接都行
./danmaku-dump https://live.bilibili.com/14709735

# 结构化输出，方便 | jq 或落盘重放
./danmaku-dump 14709735 --json

# 连其它命令（ONLINE_RANK_COUNT 之类）也打出来
./danmaku-dump 14709735 --all

# 断线不重连，直接退出
./danmaku-dump 14709735 --no-reconnect
```

消息走 stdout，连接/断开状态走 stderr。不登录也能收（uid=0 匿名连接）。

`cargo static` 需要一次性准备（ring 里有 C 代码，要个 musl 的 C 编译器）：

```bash
rustup target add x86_64-unknown-linux-musl
sudo pacman -S musl          # 提供 musl-gcc；Debian 系是 musl-tools
```

不想装也行：`cargo build --release` 走 glibc 动态链接，二进制 2.4MB、RSS 6.4MB。

### 登录（可选）

cookie 会自动从 Miyu 那个 Python 脚本 `bilibili_live_stream` 的缓存里读
（`~/.cache/bilibili-live-stream/credentials.json`，认 `XDG_CACHE_HOME` /
`BILIBILI_LIVE_STREAM_HOME`），也可以手动给：

```bash
cargo run --bin danmaku-dump -- 14709735 --cookie "SESSDATA=xxx; bili_jct=yyy"
```

## 实测踩过的坑（都进了代码）

| 坑 | 结论 |
|---|---|
| `getDanmuInfo` 直接请求返回 `-352` | 要 **WBI 签名**（`wts` + `w_rid`，mixin key 打乱表） |
| 签了名还是 `-352` | `Referer`/`Origin` 必须是 `https://live.bilibili.com`，用 `api.live.bilibili.com` 就被挡 |
| WS 认证包不带 `buvid` | 能连上、能收 `WATCHED_CHANGE` 这种房间广播，但**一条弹幕都收不到**（实测，照 blivedm 补上才行） |
| 新版 `DANMU_MSG` 解析不出来 | 现行协议字段直接放**顶层**（没有 `data`），用户信息在 `info[2]`：`[uid, uname, ...]`，老格式 `info[0][15].user` 只作兜底 |
| 弹幕包解压 | auth 包里要 `protover=2`（zlib）服务端就发 zlib，省掉 brotli 解码器；解压出来还是同样的封包序列，要递归拆 |

## 体积 / 内存（命令行 dump）

常驻后台的工具，RSS 从 12.6MB 压到 2.9MB（二进制 9.8MB → 2.6MB）：

| 步骤 | 二进制 | RSS |
|---|---|---|
| 一开始的 release | 9.8 MB | 12.6 MB |
| size profile（strip + fat LTO + `opt-level="s"` + `panic=abort`）+ 单线程 runtime | 3.7 MB | 7.8 MB |
| 砍依赖：zlib 替 brotli、ring 替 aws-lc-rs、reqwest 关掉 http2/代理/gzip 默认特性 | 2.4 MB | 6.4 MB |
| musl 静态链接（`cargo static`）+ 阻塞池线程栈 256KB | 2.6 MB | **2.9 MB** |

几个要点：

- glibc 动态链接那 3.2MB（libc + libnss_* + ld.so）**砍不掉**，只能静态链接；
  `crt-static` 静态 glibc 反而更差（glibc 代码进了自己的二进制），所以走 musl。
- tokio 阻塞池线程（DNS 解析用）默认 2MB 栈，一次解析就把这 2MB 摸进 RSS；
  `runtime()` 里压到 256KB，RSS 才稳定。
- brotli 支持是故意去掉的：认证时向服务端要 zlib，收到 brotli 会直接报错
  （真遇到再加回 `brotli` 依赖）。

## 结构

```
src/lib.rs        导出 + runtime()（单线程、256KB 栈，给命令行工具用）
src/api.rs        buvid、WBI 签名、房间信息、弹幕服务器信息、cookie 缓存读取
src/protocol.rs   封包编解码 + 消息解析（DANMU_MSG / 礼物 / SC / 点赞 / 看过 / 人气 …）
src/client.rs     WebSocket 客户端：认证、30s 心跳、指数退避重连，事件走 channel
src/bin/danmaku-dump.rs          命令行验证工具
src/bin/danmaku-overlay/main.rs  layer-shell 浮层：Wayland 事件、分数缩放、弹幕流
src/bin/danmaku-overlay/render.rs 软件渲染：tiny-skia 画暗色底板 + ab_glyph 刷字
.cargo/config.toml               cargo static 别名（musl 静态构建）
```

显示层直接消费 `DanmakuClient::run()` 吐出的 `ClientEvent` 就行，重连细节不用管。

调试变量：`DANMAKU_DEBUG=1` 打每条原始命令名；`DANMAKU_DUMP_RAW=/tmp/raw.jsonl`
把原始 JSON 逐行落盘（排查协议变动用）。

## 还没做

- 浮层：礼物/醒目留言（现在只画弹幕）、多房间
- 昵称打码：星号是**服务端下发**的（我们只是照画，代码里没有任何打码逻辑），
  按 uid 反查昵称没做——要做得先抓到一个真的打码样本
- `INTERACT_WORD_V2`（进场/关注）是 protobuf，暂时只当未知命令收着
- 发弹幕（需要登录 cookie；接口 `POST /msg/send`）
