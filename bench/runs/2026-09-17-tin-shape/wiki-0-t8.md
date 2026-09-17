# tin-shape

| corpus | docs | terms | build | index bytes | threads | seconds/workload |
|---|---|---|---|---|---|---|
| 706.2 MB | 156289 | 1218364 | 20.1 s | 361.8 MB | 8 | 30 |

| workload | queries | QPS | p50 ms | p99 ms | per kind: p99 ms (zero-result %) |
|---|---|---|---|---|---|
| conjunction+disjunction+phrase; top-10 | 1719 | 939 | 3.31 | 42.19 | conjunction 13.54 (0 %); disjunction 13.78 (0 %); phrase 51.07 (0 %) |
| conjunction+phrase; top-10 | 1146 | 847 | 4.88 | 36.31 | conjunction 11.60 (0 %); phrase 40.13 (0 %) |
| disjunction; top-10 | 573 | 3800 | 1.26 | 10.41 | disjunction 10.41 (0 %) |
| conjunction+disjunction; COUNT | 1146 | 13815 | 0.31 | 3.93 | conjunction 4.74 (0 %); disjunction 3.34 (0 %) |
| disjunction; COUNT | 573 | 9925 | 0.73 | 2.36 | disjunction 2.36 (0 %) |
