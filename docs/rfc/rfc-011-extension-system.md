# RFC-011: Extension System

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 11.1 扩展类型

| 类型              | 作用          |
| --------------- | ----------- |
| Search Provider | 搜索 API      |
| Fetch Provider  | 网页获取        |
| Site Crawler    | 网站专属爬虫      |
| Extractor       | 内容提取        |
| Embedding       | 向量生成        |
| Reranker        | 结果重排        |
| Memory          | 知识持久化       |
| Intelligence    | Query 和决策增强 |

## 11.2 三种 Extension Runtime

### Declarative

优先使用 YAML / JSON 规则。

适合：

- URL Matching。
- CSS Selector。
- XPath。
- Pagination。
- Field Extraction。
- Content Cleaning。
- Header Configuration。

### Native

Rust Trait。

第一阶段官方 Extension 随主程序编译。

不建议在初期依赖不稳定的 Rust 动态链接 ABI。

### WASM

第三方 Extension。

要求：

- WASI 沙箱。
- 显式网络权限。
- 显式文件权限。
- CPU / Memory 限制。
- 执行超时。
- ABI 版本兼容。

WASM 运行时作为后续阶段，不阻塞 MVP。

## 11.3 Extension Lifecycle

```
Discovered
   |
   v
Manifest Validation
   |
   v
Compatibility Check
   |
   v
Permission Check
   |
   v
Dependency Resolution
   |
   v
Initialize
   |
   v
Health Check
   |
   v
Registered
   |
   v
Active
```

失败进入：

- Degraded。
- Quarantined。
- Disabled。

## 11.4 Extension Versioning

每个任务启动时绑定 Extension Version Snapshot。

任务中途不允许无提示切换规则。

升级后：

- 新任务使用新版本。
- 旧任务完成或超时后释放旧版本。
- 失败时可以回滚。

## 11.5 Extension Security

- Native Extension 仅允许可信安装来源。
- WASM 通过沙箱运行。
- 扩展只能访问授权资源。
- 扩展执行必须有预算。
- 扩展不能直接修改核心 SQLite Schema。
- 扩展不能绕过成本账本。
