"use client";

/**
 * Lightweight console i18n.
 *
 * Design: the English string IS the key. `t("Overview")` returns the Chinese
 * translation when the locale is `zh`, and falls back to the original English
 * when there is no entry. This has two useful properties:
 *
 *  - Adding a new English string can never produce a blank label or crash.
 *  - Technical identifiers (API paths, metric codes, field names) stay in
 *    English on purpose, so logs and screenshots remain greppable.
 *
 * The locale is persisted in localStorage and mirrored onto `<html lang>`.
 * A `zh-*` browser language selects Chinese on first visit; an explicit user
 * choice always wins afterwards.
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";

export type Locale = "en" | "zh";

export const LOCALE_STORAGE_KEY = "searchos-locale";

type I18nValue = {
  locale: Locale;
  setLocale: (locale: Locale) => void;
  t: (text: string) => string;
};

const I18nContext = createContext<I18nValue>({
  locale: "en",
  setLocale: () => {},
  t: (text) => text,
});

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>("en");

  // Read after mount to avoid an SSR/client hydration mismatch (the same
  // approach next-themes uses for the colour scheme).
  useEffect(() => {
    let initial: Locale | null = null;
    try {
      const saved = window.localStorage.getItem(LOCALE_STORAGE_KEY);
      if (saved === "en" || saved === "zh") initial = saved;
    } catch {
      // Storage can be disabled; fall through to the browser language.
    }
    if (!initial && navigator.language.toLowerCase().startsWith("zh")) {
      initial = "zh";
    }
    if (initial) setLocaleState(initial);
  }, []);

  useEffect(() => {
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
    try {
      window.localStorage.setItem(LOCALE_STORAGE_KEY, locale);
    } catch {
      // Non-fatal: the toggle still works for this session.
    }
  }, [locale]);

  const setLocale = useCallback((next: Locale) => setLocaleState(next), []);
  const t = useCallback(
    (text: string) => (locale === "zh" ? (ZH[text] ?? text) : text),
    [locale],
  );
  const value = useMemo(() => ({ locale, setLocale, t }), [locale, setLocale, t]);

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

export function useI18n(): I18nValue {
  return useContext(I18nContext);
}

/** For non-React callers / tests that only need the translation function. */
export function translate(locale: Locale, text: string): string {
  return locale === "zh" ? (ZH[text] ?? text) : text;
}

// 中文 UI 字典。键就是英文原文，未命中的字符串自动回退英文，
// 因此新增文案不会造成空白或崩溃。技术标识（API 路径、字段名、指标码）
// 有意保留英文。
export const ZH: Record<string, string> = {
  "Overview": "总览",
  "Search": "搜索",
  "Traces": "决策轨迹",
  "Analytics": "分析",
  "Cost": "成本",
  "Knowledge": "知识库",
  "SEO": "SEO",
  "SearchOS console home": "SearchOS 控制台首页",
  "Primary navigation": "主导航",
  "SearchOS quality-first local search runtime / Analytics Read Model v1": "SearchOS 质量优先的本地搜索运行时 / Analytics Read Model v1",
  "GATEWAY ONLINE": "网关在线",
  "GATEWAY OFFLINE": "网关离线",
  "Gateway health": "网关健康状态",
  "Start the gateway with: searchos gateway start": "启动网关：searchos gateway start",
  "Toggle theme": "切换主题",
  "Switch language": "切换语言",
  "Language": "语言",
  "System overview": "系统总览",
  "Aggregated by the Analytics Read Model, not by direct table reads. Every KPI carries its denominator, definition, time range and drill-down.": "由 Analytics Read Model 聚合，而非直接读取业务表。每个 KPI 都带分母、定义、时间范围与下钻入口。",
  "Search workbench": "搜索工作台",
  "Submit a query through the shared application service. The response carries an explicit task status, quality verdict, cost, warnings and an explainable trace.": "通过共享应用服务提交查询。响应包含明确的任务状态、质量结论、成本、警告与可解释的决策轨迹。",
  "Decision traces": "决策轨迹",
  "Every search decision produces an immutable, staged trace. Inspect why local evidence was accepted or rejected, which gaps were planned, and what the final cost was.": "每次搜索决策都会产出不可变的分阶段轨迹。可查看本地证据为何被接受或拒绝、规划了哪些缺口，以及最终成本。",
  "Cost ledger": "成本账本",
  "Estimated and actual costs stay separate. Savings use an equivalent-quality external-only baseline, and reservations follow reserve, execute, settle.": "估算成本与实际账单严格分离。节省额基于等效质量的全外呼基线，预留遵循「预占 → 执行 → 结算」。",
  "RFC-026 to RFC-028: Search, Cost, Knowledge, Quality, Provider, Crawler and Infrastructure views all come from the same Analytics Read Model and standard Dashboard API.": "RFC-026 至 RFC-028：搜索、成本、知识、质量、Provider、爬虫与基础设施视图都来自同一套 Analytics Read Model 与标准 Dashboard API。",
  "Import text and files into the same knowledge store as URL fetches, then inspect versions, re-index, or delete. Documents, namespaced memory, versioned search profiles and runtime health are all here.": "把文本与文件导入与 URL 抓取相同的知识库，然后查看版本、重新索引或删除。文档、命名空间记忆、版本化搜索配置与运行时健康都在这里。",
  "Time range": "时间范围",
  "Interval": "粒度",
  "Last 7 days": "最近 7 天",
  "Last 30 days": "最近 30 天",
  "All history": "全部历史",
  "Hour": "小时",
  "Day": "天",
  "Week": "周",
  "Month": "月",
  "Minute": "分钟",
  "Refresh": "刷新",
  "UTC": "UTC",
  "Aggregation v1": "聚合 v1",
  "Read model information": "Read Model 说明",
  "Live SQL": "实时 SQL",
  "Precomputed": "预计算",
  "filter ignored": "项筛选被忽略",
  "filters ignored": "项筛选被忽略",
  "Not applicable to these views:": "以下筛选对这些视图不适用：",
  "Loading snapshot": "正在加载快照",
  "Loading ledger": "正在加载账本",
  "Gateway request failed": "网关请求失败",
  "Retry": "重试",
  "Running": "运行中",
  "Total searches": "搜索总数",
  "Local resolution": "本地解决",
  "External search": "外部搜索",
  "Quality pass rate": "质量通过率",
  "Total cost": "总成本",
  "Estimated savings": "估算节省",
  "Knowledge documents": "知识文档",
  "Evidence reuse rate": "证据复用率",
  "Estimate": "估算值",
  "Definition": "定义",
  "Drill down": "下钻",
  "Denominator:": "分母：",
  "Query": "查询",
  "Mode": "模式",
  "Strategy": "策略",
  "Status": "状态",
  "Created": "创建时间",
  "Key": "键",
  "Count": "计数",
  "Operation": "操作",
  "Provider": "Provider",
  "Workspace": "工作区",
  "Agent": "Agent",
  "Events": "事件数",
  "Micros": "微元",
  "Estimated micros": "估算微元",
  "Result": "结果",
  "Score": "得分",
  "URL": "URL",
  "Title": "标题",
  "Version": "版本",
  "Versions": "版本数",
  "Chunks": "块数",
  "Updated": "更新时间",
  "Task": "任务",
  "Reserved": "已预占",
  "Spent": "已花费",
  "Property": "属性",
  "Note": "说明",
  "File": "文件",
  "Size": "尺寸",
  "Lang": "语言",
  "Usage": "用途",
  "Stage": "阶段",
  "Output": "输出",
  "Error": "错误",
  "Items": "条目",
  "Document": "文档",
  "Changed": "内容变化",
  "Imported": "已导入",
  "Source key": "来源键",
  "Content": "内容",
  "Source": "来源",
  "Dimension": "维度",
  "Freshness": "新鲜度",
  "Coverage": "覆盖度",
  "Quality": "质量",
  "Generated at": "生成时间",
  "Timezone": "时区",
  "Workspace scope": "工作区范围",
  "Meta": "元数据",
  "Search activity": "搜索活动",
  "Recent search requests": "最近的搜索请求",
  "Decision strategies": "决策策略",
  "Metric definitions and sources": "指标定义与来源",
  "Cost by provider (estimated micros)": "按 Provider 的成本（估算微元）",
  "RFC-028 cost breakdown": "RFC-028 成本拆解",
  "No search requests recorded yet.": "还没有搜索请求记录。",
  "No search requests": "暂无搜索请求",
  "Run a query in the Search workbench to populate this table.": "在搜索工作台运行一次查询即可填充这张表。",
  "No time-series buckets yet": "暂无时间序列分桶",
  "No strategy records yet": "暂无策略记录",
  "No breakdown rows yet": "暂无拆解数据",
  "No data in this dimension": "该维度暂无数据",
  "Run a search to populate the cost ledger.": "运行一次搜索即可填充成本账本。",
  "No cost events": "暂无成本事件",
  "Run a search to populate the analytics cost breakdown.": "运行一次搜索即可填充分析成本拆解。",
  "No documents yet": "暂无文档",
  "Import text or files above, or fetch a URL with POST /extract.": "在上方导入文本或文件，或用 POST /extract 抓取 URL。",
  "No matching documents": "没有匹配的文档",
  "Import text above, or fetch a URL with POST /extract.": "在上方导入文本，或用 POST /extract 抓取 URL。",
  "No memory hits": "没有记忆命中",
  "Memory entries are created by agents through the memory service.": "记忆条目由 agent 通过记忆服务创建。",
  "No traces recorded": "没有轨迹记录",
  "Run a search in the workbench. Each task writes a staged trace to SQLite.": "在工作台运行一次搜索，每个任务都会向 SQLite 写入分阶段轨迹。",
  "Select a trace": "选择一条轨迹",
  "Choose a record on the left, or open a trace link from the Search workbench.": "在左侧选择记录，或从搜索工作台打开轨迹链接。",
  "No stage records": "没有阶段记录",
  "This trace was written by an older build. New decisions always include structured stages.": "这条轨迹由旧版本写入。新的决策始终包含结构化阶段。",
  "No response yet": "暂无响应",
  "Submit a query to see the decision path, evidence, quality verdict and cost attribution.": "提交查询以查看决策路径、证据、质量结论与成本归属。",
  "No evidence returned": "未返回证据",
  "No fake zeros. A missing metric is displayed as unavailable.": "不伪造 0。缺失指标显示为不可用。",
  "Missing values are null, not zero": "缺失值为 null，不是 0",
  "Not instrumented in MVP": "MVP 未埋点",
  "No versions": "暂无版本",
  "The document has no persisted content version.": "该文档没有已持久化的内容版本。",
  "No reservations yet": "暂无预占记录",
  "Request": "请求",
  "Strict freshness queries bypass the exact cache automatically.": "严格新鲜度查询会自动绕过精确缓存。",
  "Ask a question or paste a URL": "输入问题或粘贴 URL",
  "Profile": "配置档",
  "Versioned quality and freshness policy.": "版本化的质量与新鲜度策略。",
  "Knowledge namespace and cost attribution scope.": "知识命名空间与成本归属范围。",
  "Request id": "请求 ID",
  "Optional idempotency key": "可选幂等键",
  "Replaying a key does not create a second bill.": "重放同一键不会产生第二笔账单。",
  "Max external queries": "最大外呼查询数",
  "Hard cap on targeted gap queries.": "定向缺口查询的硬上限。",
  "Max total cost (USD)": "最大总成本（USD）",
  "Budget exhaustion returns an explicit status.": "预算耗尽会返回明确状态。",
  "Run search": "运行搜索",
  "Inspect trace": "查看轨迹",
  "Inspect decision trace": "查看决策轨迹",
  "Recent runs": "最近运行",
  "Executing": "执行中",
  "Response": "响应",
  "Decision": "决策",
  "Path": "路径",
  "Reason": "原因",
  "Replayed": "重放",
  "Evidence": "证据",
  "Summary": "摘要",
  "Raw JSON": "原始 JSON",
  "Recent traces": "最近轨迹",
  "Trace detail": "轨迹详情",
  "Selected path": "选中路径",
  "Final strategy": "最终策略",
  "Final outcome": "最终结果",
  "Stages": "阶段",
  "Metadata": "元数据",
  "Trace id": "轨迹 ID",
  "Task id": "任务 ID",
  "Profile version": "配置档版本",
  "Local candidates": "本地候选",
  "Estimated cost": "估算成本",
  "Actual cost": "实际成本",
  "Rejection reasons": "拒绝原因",
  "Baseline": "基线",
  "SearchOS": "SearchOS",
  "Savings": "节省",
  "CPQPA": "CPQPA",
  "Cost analytics": "成本分析",
  "Total estimated": "估算合计",
  "Total actual": "实际合计",
  "Cost events": "成本事件",
  "Cost per successful query": "每次成功查询成本",
  "Reuse cost views": "复用成本视图",
  "Original acquisition": "原始获取",
  "Reuse execution": "复用执行",
  "Allocated amortized": "摊销后",
  "Marginal": "边际",
  "External calls / local resolved": "外呼次数 / 本地解决",
  "Provider billing reconciliation": "Provider 账单对账",
  "Budget reservations": "预算预占",
  "By operation": "按操作",
  "By provider": "按 Provider",
  "By workspace": "按工作区",
  "By agent": "按 Agent",
  "Cost by provider": "按 Provider 成本",
  "Cost event id": "成本事件 ID",
  "Actual billed amount (USD)": "实际账单金额（USD）",
  "The estimate is never overwritten.": "估算值永远不会被覆盖。",
  "Provider billing": "Provider 账单",
  "Local measurement": "本地测量",
  "User import": "用户导入",
  "Billing data flips the report from estimate to actual.": "账单数据会把报告从估算切换为实际。",
  "Reconcile": "对账",
  "cost_<uuid>": "cost_<uuid>",
  "Freshness status": "新鲜度状态",
  "Coverage status": "覆盖度状态",
  "Source status": "来源状态",
  "Failure classes": "失败类别",
  "Knowledge growth": "知识增长",
  "Knowledge summary": "知识摘要",
  "Provider usage / cost": "Provider 用量 / 成本",
  "Provider health": "Provider 健康",
  "Crawler summary": "爬虫摘要",
  "Infrastructure": "基础设施",
  "Search requests": "搜索请求",
  "Strategy distribution": "策略分布",
  "Read Model / cursor-ready": "Read Model / 支持游标",
  "Days with documents": "有文档的天数",
  "Embeddings": "向量",
  "Database": "数据库",
  "Index": "索引",
  "Consistent": "一致性",
  "Providers": "Providers",
  "Crawler extensions": "爬虫扩展",
  "Extensions": "扩展",
  "Auth": "鉴权",
  "Rate limit": "限流",
  "Total runs": "总运行数",
  "Successful": "成功",
  "Failed": "失败",
  "SQLite size": "SQLite 大小",
  "Indexed chunks": "已索引块数",
  "Growth": "增长",
  "Profiles": "配置档",
  "Health": "健康",
  "Crawlers": "爬虫",
  "Read model": "Read Model",
  "Import knowledge": "导入知识",
  "Document library": "文档库",
  "Document detail": "文档详情",
  "Find by URL or title": "按 URL 或标题查找",
  "URL or keyword": "URL 或关键词",
  "Matches canonical URL or title text.": "匹配规范化 URL 或标题文本。",
  "Find": "查找",
  "Search memory": "搜索记忆",
  "Namespaced entries visible to the current workspace only.": "仅显示当前工作区可见的命名空间条目。",
  "Entry": "条目",
  "Knowledge Read Model": "知识 Read Model",
  "Text": "文本",
  "Files": "文件",
  "Title / URL": "标题 / URL",
  "Canonical URL": "规范化 URL",
  "Versions / chunks": "版本 / 块数",
  "Version history": "版本历史",
  "Content hash": "内容哈希",
  "Fetched": "抓取时间",
  "Extractor": "抽取器",
  "Import text": "导入文本",
  "Import file": "导入文件",
  "Paste the text you want searchable…": "粘贴希望可被检索的文本…",
  "Optional display title": "可选显示标题",
  "Re-index": "重新索引",
  "Delete": "删除",
  "Re-index rewrites only the full-text index entries. Metadata, the version row and the chunk rows are never rewritten, so the version count is unchanged.": "重新索引只重写全文索引条目。元数据、版本行与块行都不会被改写，因此版本数不变。",
  "Re-indexed": "已重新索引",
  "no longer returned by document search": "不再出现在文档检索结果中",
  "Choose at least one file first.": "请先选择至少一个文件。",
  "Pick one or more text files. One file uses /ingest/file; several use /ingest/batch.": "选择一个或多个文本文件。单文件走 /ingest/file，多个走 /ingest/batch。",
  "Re-importing the same key + unchanged content is a no-op.": "相同来源键 + 内容未变化时重复导入是空操作。",
  "Imported into the same knowledge store as URL fetches: same versioning, chunking, workspace isolation and rebuild path.": "导入到与 URL 抓取相同的知识库：同样的版本化、分块、工作区隔离与重建路径。",
  "All sources": "全部来源",
  "Web (fetched)": "网页（抓取）",
  "Local (ingested)": "本地（导入）",
  "Local text": "本地文本",
  "Local file": "本地文件",
  "Live documents are scoped to this workspace; global (": "实时文档限定于该工作区；全局（",
  ") rows stay visible.": "）行始终可见。",
  "workspace scope": "工作区范围",
  "SOCIAL CARD PREVIEW": "社交卡片预览",
  "X / TWITTER - SUMMARY_LARGE_IMAGE": "X / TWITTER - SUMMARY_LARGE_IMAGE",
  "GITHUB - REPO SOCIAL PREVIEW": "GITHUB - 仓库社交预览",
  "META TAGS - THIS PAGE": "META 标签 - 本页",
  "OG ASSETS": "OG 资源",
  "SHARE CHECKLIST": "分享检查清单",
};
