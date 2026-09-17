# tin-shape

| corpus | docs | terms | build | index bytes | threads | seconds/workload |
|---|---|---|---|---|---|---|
| 706.2 MB | 156289 | 1218364 | 21.6 s | — | 4 | 30 |

| workload | queries | QPS | p50 ms | p99 ms | per kind: p99 ms (zero-result %) |
|---|---|---|---|---|---|
| conjunction+disjunction+phrase; top-10 | 1719 | 798 | 2.19 | 22.85 | conjunction 8.58 (0 %); disjunction 8.58 (0 %); phrase 25.75 (0 %) |
| conjunction+phrase; top-10 | 1146 | 655 | 3.54 | 21.95 | conjunction 7.87 (0 %); phrase 23.76 (0 %) |
| disjunction; top-10 | 573 | 2503 | 0.98 | 7.39 | disjunction 7.39 (0 %) |
| conjunction+disjunction; COUNT | 1146 | 9224 | 0.24 | 1.81 | conjunction 3.13 (0 %); disjunction 1.72 (0 %) |
| disjunction; COUNT | 573 | 5834 | 0.64 | 1.69 | disjunction 1.69 (0 %) |
