# 中文分词与 CJK 检索

中文没有空格，全文检索必须处理 CJK 连续文本。字符 bigram 把连续的汉字切成重叠二元组，是 Lucene 与 Elasticsearch 常用的中文召回方案。ngram 解析器也能支持中文检索。
