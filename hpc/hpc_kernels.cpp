#include <algorithm>
#include <cmath>
#include <cstddef>
#include <cstdint>
#include <cblas.h>
#include <omp.h>
#include <queue>
#include <vector>
#include <utility>

extern "C" {

void hpc_normalize_rows(float* data, std::size_t rows, std::size_t cols) {
    #pragma omp parallel for schedule(static)
    for (std::size_t r = 0; r < rows; ++r) {
        float* row = data + r * cols;
        double sum = 0.0;
        #pragma omp simd reduction(+:sum)
        for (std::size_t c = 0; c < cols; ++c) sum += double(row[c]) * double(row[c]);
        const float inv = sum > 0.0 ? 1.0f / std::sqrt(float(sum)) : 0.0f;
        #pragma omp simd
        for (std::size_t c = 0; c < cols; ++c) row[c] *= inv;
    }
}

void hpc_sgemv_scores(const float* matrix, const float* query, float* scores,
                      std::size_t rows, std::size_t cols) {
    cblas_sgemv(CblasRowMajor, CblasNoTrans,
                static_cast<int>(rows), static_cast<int>(cols),
                1.0f, matrix, static_cast<int>(cols), query, 1,
                0.0f, scores, 1);
}

void hpc_weighted_fuse(const float* semantic, const float* lexical,
                       const float* evidence, const float* freshness,
                       float* out, std::size_t n,
                       float ws, float wl, float we, float wf) {
    #pragma omp parallel for simd schedule(static)
    for (std::size_t i = 0; i < n; ++i) {
        out[i] = semantic[i] * ws + lexical[i] * wl + evidence[i] * we + freshness[i] * wf;
    }
}

void hpc_topk_indices(const float* scores, std::size_t n, std::size_t k, std::uint32_t* out) {
    // Parallel block-local top-k candidates, followed by deterministic merge.
    const int threads = omp_get_max_threads();
    const std::size_t local_k = std::min(k, n);
    if (local_k == 0) return;
    std::vector<std::vector<std::pair<float,std::uint32_t>>> locals(threads);
    #pragma omp parallel
    {
        int tid = omp_get_thread_num();
        auto& v = locals[tid];
        v.reserve(local_k);
        // `better` makes the priority queue expose the worst retained candidate,
        // keeping per-thread memory bounded by k instead of the full index size.
        auto better = [](const auto& a, const auto& b) {
            return a.first != b.first ? a.first > b.first : a.second < b.second;
        };
        std::priority_queue<std::pair<float,std::uint32_t>,
                            std::vector<std::pair<float,std::uint32_t>>,
                            decltype(better)> heap(better);
        #pragma omp for nowait schedule(static)
        for (std::size_t i=0; i<n; ++i) {
            const auto candidate=std::make_pair(scores[i],static_cast<std::uint32_t>(i));
            if (heap.size()<local_k) heap.push(candidate);
            else if (better(candidate,heap.top())) { heap.pop(); heap.push(candidate); }
        }
        while (!heap.empty()) { v.push_back(heap.top()); heap.pop(); }
    }
    std::vector<std::pair<float,std::uint32_t>> merged;
    for (auto& v : locals) merged.insert(merged.end(), v.begin(), v.end());
    std::sort(merged.begin(), merged.end(), [](const auto& a,const auto& b){return a.first!=b.first?a.first>b.first:a.second<b.second;});
    for (std::size_t i=0; i<local_k; ++i) out[i] = merged[i].second;
}

}
