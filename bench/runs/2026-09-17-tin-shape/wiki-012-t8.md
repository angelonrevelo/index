# tin-shape

| corpus | docs | terms | build | index bytes | threads | seconds/workload |
|---|---|---|---|---|---|---|
| 1844.7 MB | 468867 | 2336183 | 65.5 s | 949.7 MB | 8 | 30 |

| workload | queries | QPS | p50 ms | p99 ms | per kind: p99 ms (zero-result %) |
|---|---|---|---|---|---|
| conjunction+disjunction+phrase; top-10 | 1719 | 499 | 5.15 | 81.40 | conjunction 22.67 (0 %); disjunction 22.64 (0 %); phrase 95.41 (0 %) |
| conjunction+phrase; top-10 | 1146 | 363 | 9.80 | 86.38 | conjunction 23.41 (0 %); phrase 98.20 (0 %) |
| disjunction; top-10 | 573 | 1756 | 2.34 | 24.48 | disjunction 24.48 (0 %) |
| conjunction+disjunction; COUNT | 1146 | 5667 | 0.77 | 8.37 | conjunction 14.89 (0 %); disjunction 5.81 (0 %) |
| disjunction; COUNT | 573 | 3774 | 1.94 | 6.02 | disjunction 6.02 (0 %) |
