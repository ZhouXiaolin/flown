# `/model` 命令 + 通用 `TableView`/Overlay — Design Spec

- **Status**: Draft (2026-06-19)
- **Related**: ADR-0001 (async `ExtensionContext` + command proxy);
  `2026-06-17-btw-extension-design.md`(overlap 的首个消费方,本次把它的
  overlap 概念提炼成通用 UI 抽象)。
- **Convention**: Solaren。给人读的 prose 用中文;标识符保持英文。

## 1. Goal

`/model` —— 一个 slash command,唤起一个 **浮层框**(上下左右各 1/8 边距,
盖在主对话之上,主对话不替换)。两步选择:

1. 模型列表(按 `config.providers` 分 section)→ Enter →
2. thinking 强度列表(按所选模型支持的 levels)→ Enter → 应用并关闭。

应用 = 调 `harness.set_model()` + `set_thinking_level()`(目标:主层 harness),
statusline 经响应式信号自动同步。**全程不 fork session、不碰 agent 结构。**

## 2. 设计起点:overlap 当前是「带 fork 的会话层」,不是 UI 抽象

研究现状后得出的关键诊断 —— 现有 `OverlapOptions` +
`RuntimeControl::open_overlap` + `ConversationLayer{kind:Overlap}` 把三件事糅
在一起:

1. **UI 摆放** —— push 一个 layer 成 `active()`,组件重读 `active().state`。
2. **Session 生命周期** —— fork 主 session 历史到内存 session。
3. **Agent 生命周期** —— 建 harness、绑 driver、起 event pump。

`OverlapOptions` 里的 `initial_prompt`/`single_instance_key`/`slash_commands`
全是 conversation-fork 的策略,跟 UI 无关。所以 overlap 现在**不是 UI 抽象**。

更具体的现状:**btw 的 overlap 不是「浮层框」,是「整屏替换」**。`app.rs` 的
`view!` 只有 `Transcript()/StatusLine()/InputEditor()`,都读
`stack.active().state`;btw 一 push,这三个组件用 btw 的 UiState 重绘,主对话被
完全替换。`position_absolute`/`inset_*`(iodilos 自带,见 `examples/overlap.rs`)
flown 一次没用过。

### 2.1 本设计对 overlap 的重新定义

overlap 重新定位为 **通用 UI 浮层抽象**,几何由参数决定:

```rust
pub enum OverlayGeometry {
    /// inset 0 + bg Reset = 整屏替换(btw 现形态)。
    FullBleed,
    /// 四周 ratio 比例边距(model: 0.125)。
    Inset { ratio: f32 },
}
```

btw 的整屏替换 = `FullBleed`(`background: Color::Reset`)。model 的浮层框 =
`Inset { 0.125 }`。**两者不是两种形态,是同一浮层的两个几何参数。**

会话 fork 能力从 overlap 概念里抽出去,成为 btw 独有的 capability(§5.3)。

## 3. 两层抽象(全通用 UI 原语,非 model 特化)

| 件 | 是什么 | 归属 | 谁用 |
|---|---|---|---|
| **`TableView`(新)** | section-header + rows + 选中光标 + 居中滚动的纯渲染组件 | **iodilos** | model 浮层、editor 补全、未来一切选择器 |
| **`OverlayBox`(新)** | `position_absolute` + 几何参数 + 可 dismiss 的浮层容器,内含一个 content slot | **iodilos** | btw、model 共用 |

会话 fork capability(§5.3)是 btw 独有的,不进上表。

### 3.0 为什么不要 `ModalDriver` trait(显式否决)

曾考虑加一个「选择策略 trait」(`ModalDriver`:阶段状态机 + 数据供给)来「通用化
选择器」。**否决**,原因:

1. **数据供给已被 cell-based TableView 吃掉**。`Signal<Vec<TableSection>>` +
   `CellFactory` 直接表达供给,`CellFactory` 本身就是闭包(最轻量的多态),再包
   trait 是重复抽象。
2. **阶段状态机不需要 trait**。model 的「选模型 → thinking → apply」就是一个
   `phase` 枚举 + 几个 `RwSignal` + 按键 handler 里的 `match phase`。套 trait 只
   会把差异(各选择器阶段数/语义不同)硬塞进 `ConfirmOutcome`/`BackOutcome` 枚举。
3. **与 btw 不对称**。`btw.rs` 直接写 fork 逻辑,没有 `BtwDriver` trait;同理
   model 不需要 `ModelDriver` trait。

**结论:各选择器(未来 `/theme`、`/skill`)各写自己的 overlay 组件,共享的是
`TableView`/`OverlayBox` 这些真正的 UI 原语,不是策略 trait。** model 的全部逻辑
(状态 + 数据 + 按键 + apply)集中在一个 `ModelOverlay` 组件里,放 `model.rs`。

### 3.1 关键收益:`CompletionMenu` 退化为 `TableView` 的一个组装

`CompletionMenu`(iodilos 现有,扁平、有选中、无 section)只被 flown 在
`editor.rs:46-48`(slash popup 补全)用一次。它本质是「一个 section、无 header、
`max_visible=8`、cell 工厂渲染 label+description 的 TableView」。引入 cell-based
`TableView` 后,`CompletionMenu` **重构为 `TableView` 的预设组装**(一个固定
的 `CellFactory` 把 `CompletionItem` 渲染成两行文本),消除两份选中/滚动逻辑。

## 4. `TableView` 组件(iodilos,纯 UI,cell-based)

参考 iOS `UITableView` 的 **数据源(`numberOfSections` /
`cellForRowAt`)+ `selectedIndexPath`** 那层。核心是 **cell 抽象**:tableView
不认识行数据类型、不认识 cell 长什么样,只管 section 结构 + 选中态 + 滚动;每行
的渲染交给调用方提供的 `CellFactory`(等价于 iOS 的
`tableView(_:cellForRowAt:)` + `dequeueReusableCell`)。这样不同数据类型(Book /
Movie / Model / ThinkingLevel)各自有自己的 cell 样式,由调用方在工厂里分支,与
iOS「按类型 dequeue 不同 cell」对齐。

```rust
/// tableView 的一行身份。只承载稳定身份,**不含任何渲染字段**。
/// 调用方用自己的数据结构(在闭包外持有)按 key 取真实数据。
#[derive(Clone)]
pub struct TableRow {
    pub key: String,              // 稳定身份,For 式 keyed diff
}

/// 一个 section。header 也是 Node,样式/内容完全由调用方决定(不限于文本)。
pub struct TableSection {
    pub header: Option<Node>,     // None = 无标题行(CompletionMenu 式)
    pub rows: Vec<TableRow>,
}

/// tableView 交给 cell 工厂的上下文。
pub struct CellContext<'a> {
    pub key: &'a str,             // 当前 row 的 key(调用方据此取自己的数据)
    pub section_idx: usize,
    pub row_idx: usize,
    pub selected: bool,           // tableView 注入选中态;cell 决定怎么高亮
}

/// cell 工厂:对每个可见 row 调用一次,返回该 row 的 Node。
/// 这是 iOS `cellForRowAt` + dequeue 的等价物 —— tableView 不缓存 cell
/// 实例(终端 UI 量级小,无需 cell 复用池),每次 render 调工厂现造。
pub type CellFactory = Rc<dyn Fn(&CellContext) -> Node>;

pub struct TableViewProps {
    pub sections: Signal<Vec<TableSection>>,
    pub cell_factory: CellFactory, // 每行怎么渲染,归调用方
    /// 扁平化后的全局光标:在「所有 section 的 rows 拼接序列」上的索引,
    /// 跨 section 连续移动(跳过 header 行)。
    pub selected: Signal<usize>,
    /// 视口可见行数;光标靠近视口边时居中滚动(参考 pi-mono
    /// model-selector 的 startIndex = clamp(selected - max_visible/2))。
    pub max_visible: usize,
}
```

调用方典型用法(model.rs 里):

```rust
// model 特化数据,在闭包外持有
let items: Rc<HashMap<String, ModelItem>> = /* ... */;
let cell_factory: CellFactory = Rc::new(move |ctx| {
    let item = items.get(ctx.key).expect("key from sections");
    match item {
        ModelItem::Model(m) => model_cell(m, ctx.selected),    // label + provider badge
        ModelItem::ThinkingLevel(l) => level_cell(l, ctx.selected),
    }
});
// sections 用同样的 key 集合;tableView 对每个可见 row 调 cell_factory。
```

**组件职责**:遍历 section、渲染 header Node、对每个可见 row 调
`cell_factory` 拿 Node(把 `selected` 经 `CellContext` 注入);把全局 `selected`
投影回 (section, row);视口随光标居中滚动。

**组件不管**:按键(` ↑/↓` 由调用方写 `selected.set(...)`)、确认语义、过滤、
cell 内部样式。它是被动渲染器 + cell 调度器。

### 4.1 为什么不要 cell 复用池(dequeue)

iOS 的 `dequeueReusableCell` 是为「千行列表 + cell 实例昂贵」优化。终端 tableView
可见行数 ≤ `max_visible`(几十),cell 是轻量 Node,每次 render 现造的成本可忽
略。引入复用池会带来「cell 类型注册表 + identity 映射」的复杂度,收益为零。
`CellFactory` 每次返回新 Node 即可 —— 简单且正确。

### 4.2 全局光标 ↔ (section, row) 映射

TableView 在 `create_effect` 里把 `sections` 扁平成
`(section_idx, row_idx, key)` 序列(只数 rows,不数 header),据此:
- 渲染时计算当前 row 是否为 `selected` 对应项,经 `CellContext.selected` 传给
  cell 工厂(cell 自己决定高亮:背景色、前缀箭头等)。
- 居中滚动:由扁平序列长度与 `max_visible` 算 `[start, end)` 视口,只对切片内的
  row 调 `cell_factory`。

调用方(flown 侧)只维护一个全局 `usize` 光标,不感知 section 结构。

## 5. `OverlayBox` 组件(iodilos,纯 UI)

一个浮层容器。它本身不追踪「当前活动浮层」(那是 flown 的
`OverlayStack`,§6)—— 它只是「按几何参数摆放 + 渲染背景 + 装 content」。

```rust
pub struct OverlayBoxProps {
    pub geometry: OverlayGeometry,  // FullBleed | Inset { ratio }
    pub background: Color,          // FullBleed 用 Reset(整屏覆盖);Inset 可自定义
    pub border: Borders,
    pub border_style: BorderStyle,
    pub content: Node,              // 唯一 content slot
}
```

实现 = `Node::new_view()` + `set_position_absolute(())` + 按 `geometry` 调
`set_inset_top/bottom/left/right`(FullBleed 全 0;Inset 按 `ratio * 100` 设 percent
inset —— 但 taffy inset 不直接吃 percent,见 §5.1)。`draw` 里清底层 glyph + 填
背景(复用现有 `View::draw` 的 background-clear 逻辑,`view.rs:442`)。

### 5.1 inset percent 的实现细节

taffy `inset` 支持 `LengthPercentageAuto::Percent`。iodilos `set_inset_top` 等目
前只吃 `length`(绝对值)。**本次为 inset 系列补 percent 变体**
(`set_inset_top_percent` 等),与现有 `set_width_percent` 一致。model 的 1/8 边
距用 `inset_percent(12.5)`。

(若 taffy 对 absolute + percent inset 的支持有限,退路:OverlayBox 在
`create_effect` 里读窗口尺寸信号,把 percent 换算成绝对 `length` 再 set。优先试
percent 直连。)

## 6. flown 侧:`OverlayStack` + model.rs

### 6.1 `OverlayStack`(替代 ConversationStack 兼管 UI 的部分)

一个 iodilos context 持有的、追踪当前顶层浮层的 reactive 状态(0 或 1 个):

```rust
pub struct OverlayStack {
    /// 当前活动浮层。空 = 主对话可见。
    active: RwSignal<Option<Rc<ActiveOverlay>>>,
}
pub struct ActiveOverlay {
    pub geometry: OverlayGeometry,
    pub dismissible: bool,
    /// 浮层内容工厂:每次 push 时调用,返回 content Node。
    pub content: Rc<dyn Fn() -> Node>,
    /// 关闭回调(可选):btw 用来做 fork 会话 teardown;model 无需。
    pub on_close: Option<Box<dyn FnOnce()>>,
}
```

`App` 的 `view!` 在主布局之外,条件渲染顶层 OverlayBox:读 `overlay_stack.active()`,
有就 `OverlayBox { content: overlay.content() }`,无就什么都不加。主对话始终在
背后渲染(btw 的 FullBleed + Reset 背景靠覆盖盖住它;model 的 Inset 露出 1/8 边
距可见)。

Ctrl+C 当 `active.is_some() && dismissible` 时关闭顶层浮层(调 `on_close` 再
`pop`),否则走原 main 逻辑。**这条路由替换 `app.rs:99-120` 现有的 Ctrl-C 分支。**

### 6.2 model 浮层 = 直接的 `ModelOverlay` 组件(无 trait)

model 的全部逻辑(状态 + 数据 + 按键 + 阶段 + apply)集中在一个 `ModelOverlay`
组件里,放 `model.rs`(见 §7)。它内部用 iodilos 的 `OverlayBox(Inset)` + 标题
行 + 搜索 `TextArea` + `TableView` 组装,没有策略 trait、没有 `Box<dyn>`、没有
`ConfirmOutcome`/`BackOutcome` 枚举。阶段流转就是按键 handler 里的 `match phase`。

未来 `/theme`、`/skill` 选择器同理各写自己的 `ThemeOverlay`/`SkillOverlay` 组件;
它们共享的是 `TableView`/`OverlayBox` 这些 UI 原语,不是策略 trait(理由见 §3.0)。

## 7. `model.rs` —— 全部 model 策略集中于此

遵循 btw 模式(策略全在 `btw.rs`,TUI 只认通用 overlap):model 的所有特化逻辑
都在 `core/extensions/model.rs`。

### 7.1 `ModelExtension`(注册)

```rust
pub struct ModelExtension;
impl Extension for ModelExtension {
    fn name(&self) -> &'static str { "model" }
    fn register(&self, api: &mut ExtensionApi) {
        api.register_command("/model", CommandMeta::simple(
            "Choose model and thinking intensity (overlay)"
        ), Arc::new(|_inv, ctx| Box::pin(async move {
            ctx.conversation.open_model_overlay().await
        })));
    }
}
```

`ctx.conversation.open_model_overlay()` →
`RuntimeCommand::OpenModelOverlay` → iodilos 侧 RuntimeControl 直接构造
`ModelOverlay`(无需查注册表:只有 model 一个消费方)并 push 进 `OverlayStack`。

### 7.2 `ModelOverlay` 组件(model 全部策略集中于此)

一个 iodilos 组件,持有全部 model 特化状态。两阶段两种 cell 类型,按 phase 切换
`CellFactory`。

```rust
enum ModelPhase { Model, Thinking }

struct ModelOverlay {
    config: Config,
    harness: Arc<AgentHarness>,      // 主层 harness(apply 目标)
    current: Option<Model>,          // 开浮层时快照的当前模型(标 ✓)
    phase: RwSignal<ModelPhase>,
    query: RwSignal<String>,         // 搜索框
    selected: RwSignal<usize>,       // 全局光标
    picked: RwSignal<Option<Model>>, // 第一阶段选中,传给第二阶段
    // 数据快照:每次重建 sections 后同步更新,供 cell_factory 按 key 取真实数据。
    // 两者同源 —— sections 的每个 key 都在这两个 map 里。
    model_items: Rc<RefCell<HashMap<String, Model>>>,
    level_items: Rc<RefCell<HashMap<String, ThinkingLevel>>>,
}
```

- **`build_sections()`**(由 `create_effect` 在 `query`/`phase` 变化时重算,产 key 身份):
  - `Model` 阶段:providers = `config.providers.keys()`;每 provider 一 section,
    header Node = provider 名标题;rows = `flown_ai::get_models(provider)` 经
    fuzzy 匹配 `query`(匹配 `id`/`name`/`provider/id`)后,每行 `key =
    format!("{provider}/{id}")`。重建 `model_items` map。
  - `Thinking` 阶段:单 section,rows = `get_supported_thinking_levels(&picked)`,
    `key = format!("{level:?}")`。重建 `level_items` map。
- **`cell_factory()`**(读 `phase` 信号,按阶段返回不同工厂,渲染不同 cell 类型):
  - `Model` 阶段:工厂闭包捕获 `model_items`,按 `ctx.key` 取 `Model`,
    渲染 model cell(label = `model.id` / `model.name`,右侧 provider badge,
    当前模型行经 `models_are_equal(&current, &m)` 判定加 "✓",选中态按
    `ctx.selected` 高亮)。
  - `Thinking` 阶段:工厂闭包捕获 `level_items`,渲染 level cell(主文本 =
    level 名,描述 = pi-mono thinking-selector 文案 `{off: "No reasoning", ...}`,
    当前 level 加 "✓")。
- **按键 handler**(`on_key`,直接 match,无 trait):
  - ` ↑/↓`:改 `selected`(跨 section 连续,跳过 header),TableView 自动重绘。
  - Enter(按 `phase` + 当前 `selected` 对应的 key 分支):
    - `Model` 阶段:从 `model_items` 取 `Model`;若
      `get_supported_thinking_levels` 只有 `[Off]` → `apply(model, Off)` 并关
      浮层;否则 `picked.set(Some(model))`、`phase.set(Thinking)`、
      `selected.set(0)`、`query.clear()`(TableView effect 自动重算成 thinking
      列表)。
    - `Thinking` 阶段:从 `level_items` 取 level,`apply(picked, level)` 并关浮层。
  - Esc(按 `phase` 分支): `Thinking` → `phase.set(Model)`、`selected.set(0)`
    (回模型列表);`Model` → 关浮层。
  - 其他键 → 喂给搜索 `TextArea`(`query` 变 → sections effect 重算)。
- **apply(model, level)**: `tokio::spawn(async move {
  h.set_model(model).await; h.set_thinking_level(level).await; })`。
  harness Arc 已在 iodilos context(`Option<Arc<AgentHarness>>`,
  `runtime.rs:254`),Send 安全。关浮层 = `overlay_stack.pop()`(model 无 teardown,
  无 `on_close`)。

> 「sections 产 key、cell_factory 按 key 取数据、两者同源」是这个组件的不变量:
> `build_sections()` 重建 map,`cell_factory()` 闭包捕获同一 map。阶段切换时
> `phase` 信号变 → 两个 effect 都重跑,map 和工厂一起换,不会出现「key 在 sections
> 里但 cell_factory 查不到」。

## 8. btw 迁移到通用 Overlay(本次一并完成)

btw 从「active-swap 整屏替换」改成 `Overlay(FullBleed) + 会话 fork capability`。

### 8.1 会话 fork capability(从 `open_overlap` 抽出)

`RuntimeControl::fork_conversation(prompt: Option<String>)` —— 现
`open_overlap` 里建 harness、bind driver、起 pump、push layer 的那段逻辑。fork
完返回一个「装着 transcript 的 content Node 工厂」,交给 OverlayBox。

### 8.2 btw 改动

- `btw.rs` 的 `open_btw_overlap` 改调
  `ctx.conversation.fork_conversation(prompt)`(新 RuntimeCommand 变体),不再
  用 `OverlapOptions`。
- btw 浮层 = `OverlayBox(FullBleed, bg Reset)` + content = btw transcript 组件。
  transcript 组件读 fork 出来的 UiState(不再读 `stack.active().state`)。
- `ConversationStack` 的 `active_index`/active-swap 机制 **逐步退役**:本期
  btw 迁过去后,所有浮层都走 OverlayStack;ConversationStack 退回成「主对话 +
  按需持有 fork 出的 harness 句柄」的薄层。本期以 btw 跑通 OverlayStack 为目
  标,ConversationStack 的最终形态在实现中收敛。

### 8.3 风险与边界

btw 刚落地、有测试(`conversation.rs` 的 `pop_active_*` 等)。迁移 active-swap →
OverlayStack 会动到这些。本期把 btw 测试更新到 OverlayStack 模型;若迁移中暴露
出 OverlayStack 表达不了的 btw 语义,记录为 follow-up,**不**为 btw 回退通用抽象。

## 9. statusline 同步(通用,非 model 特化)

`runtime.rs:475` 的 `translate_event` 当前用 `_ => {}` 兜底,丢弃了
`ModelUpdate`/`ThinkingLevelUpdate`。新增两个分支:

```rust
AgentHarnessEvent::ModelUpdate { model, .. } => {
    state.status.update(|s| {
        s.model = format!("{}/{}", model.provider, model.id);
        s.provider = model.provider.to_string();
    });
}
AgentHarnessEvent::ThinkingLevelUpdate { level, .. } => {
    state.status.update(|s| s.thinking_level = format!("{:?}", level).to_lowercase());
}
```

任何模型变更都自动同步 statusline(StatusLine 组件读 `status` 信号响应式重绘)。
**这是通用修复,不属于 model 特化 UI。**

## 10. 涉及文件

| 仓库 | 文件 | 改动 |
|---|---|---|
| **iodilos** | **新** `components/table_view.rs` | `TableView`/`TableSection`/`TableRow`/`TableViewProps`。 |
| iodilos | **新** `components/overlay_box.rs` | `OverlayBox`/`OverlayGeometry`/`OverlayBoxProps`。 |
| iodilos | `components/view.rs` | inset 系列补 percent 变体(§5.1)。 |
| iodilos | `components/completion_menu.rs` | 重构为 `TableView` 的预设组装(§3.1)。 |
| iodilos | `lib.rs` / `prelude` | 导出新组件。 |
| iodilos | **新** `examples/table_view.rs` | tableView demo(section 分组 + 选中)。 |
| **flown** | **新** `core/extensions/model.rs` | `ModelExtension` + `ModelOverlay` 组件(全部 model 策略:状态 + 数据 + 按键 + 阶段 + apply)。 |
| flown | `core/extensions/types.rs` | `RuntimeCommand::OpenModelOverlay`、`::ForkConversation { prompt }`;`ConversationCapability` 加 `open_model_overlay`/`fork_conversation`。 |
| flown | `core/extensions/mod.rs` | `build_runner` 加 `ModelExtension`。 |
| flown | **新** `tui/overlay_stack.rs` | `OverlayStack`/`ActiveOverlay`。 |
| flown | `tui/conversation.rs` | 抽出 `fork_conversation`;`RuntimeControl::open_model_overlay`(直接构造 `ModelOverlay` 并 push,无注册表)。 |
| flown | `tui/runtime.rs` | mount 建 `OverlayStack`;`spawn_runtime_command_pump` 处理 `OpenModelOverlay`/`ForkConversation`;`translate_event` 加 ModelUpdate/ThinkingLevelUpdate 分支。 |
| flown | `tui/components/app.rs` | `view!` 渲染主布局 + 条件渲染顶层 `OverlayBox`;Ctrl+C 浮层优先关闭分支。 |
| flown | `core/extensions/btw.rs` | 改用 `Overlay(FullBleed) + fork_conversation`(§8)。 |
| flown | `tui/components/editor.rs` | slash popup 改用 `TableView` 组装(CompletionMenu 退役后)。 |

## 11. 线程与生命周期不变量(承重约束)

1. **`ModelOverlay` 是 iodilos 线程本地**(持有 `Config` clone + `Arc<AgentHarness>` —— Arc 可跨线程,但组件本身通过 iodilos context 提供,不 Send)。由 RuntimeControl 在 iodilos 线程构造并 push,跟 btw 的 fork 路径同性质。
2. **apply 跨线程**:`harness.set_model`/`set_thinking_level` 是 async,在 `tokio::spawn` 里跑;结果经 `ModelUpdate`/`ThinkingLevelUpdate` 事件回流 iodilos(经主 event pump),不直接在组件里等。
3. **OverlayStack 是 iodilos-local `Rc`**(同 ConversationStack),不跨线程。
4. **fork_conversation 的 teardown 顺序不变**:unsubscribe → send Shutdown → drop(承 btw spec §4.4 的 load-bearing 顺序)。
5. **主层 harness 是 `/model` apply 的唯一目标**:driver 持 `main_layer().harness`,不碰 overlap 的 fork harness。

## 12. 测试策略

- **iodilos 单元**:`TableView` 扁平化映射(selected → (section,row))、视口居中切片、cell_factory 按 key 调用且 `selected` 正确注入(用 fake factory 断言每个可见 row 被调一次、选中行的 `ctx.selected==true`)、keyed diff;`OverlayBox` 几何(FullBleed inset=0、Inset percent 换算)。新 `examples/table_view.rs` 作手动验收。
- **flown 单元(无 TUI)**:`ModelOverlay` 的 `build_sections`/按键阶段流转(Enter/Esc 的 phase 切换),用 fake `config.providers` + 真 `get_models`;Enter 对 `[Off]`-only 模型直接 apply+关、对多 level 模型进 Thinking 阶段;fuzzy 过滤命中。把纯逻辑(sections 计算、fuzzy、phase 流转)抽成不依赖 Node 的函数,便于无 TUI 单测。
- **flown 集成**:mount `OverlayStack`,push 一个 `ModelOverlay`,模拟 ` ↑/↓`/Enter/Esc,断言 phase/picked 状态机;apply 后断言 `translate_event(ModelUpdate)` 更新了 `status.model`。
- **btw 迁移回归**:原 `conversation.rs` 的 `pop_active_*` 测试更新到 OverlayStack 模型;手动 smoke `/btw <msg>` 仍能 fork + 流式 + Ctrl+C 退出回主。

## 13. 范围外(Out of scope)

- **`/login`**:配置 provider、写回 `config.providers`。本期 `/model` 只读现 config;`/login` 是后续独立特性。
- **修改 `config.providers` 的模型来源**:模型仍只来自 `models.generated.json`(已与 pi-mono 同步,968 models / 35 providers,无需拷贝)。config 只决定显示哪些 provider。
- **ConversationStack 的彻底删除**:本期以 btw 跑通 OverlayStack 为目标;ConversationStack 的最终形态(是否完全并入 OverlayStack)在实现中收敛,不强行在本期清零。
- **nesting overlay-within-overlay**:OverlayStack 本期支持 0/1 顶层浮层;多层栈结构留 follow-up。
- **`/theme`/`/skill` 选择器**:作为 `TableView`/`OverlayBox` 的未来消费方验证通用性,不在本期实现(各写自己的 overlay 组件,无策略 trait,见 §3.0)。
