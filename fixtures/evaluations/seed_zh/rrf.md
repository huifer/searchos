# RRF 融合排序

Reciprocal Rank Fusion 用各路召回结果的名次计算分数，公式为 1 除以 k 加名次。默认 k 取 60。RRF 不要求不同检索器的分数可比，适合融合 BM25 与向量候选。
