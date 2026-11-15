# --sim 运行记录（由脚本生成，mtkclient-rs da362af）

## $ mtkclient-rs --sim v5-clean printgpt
```
[sim] 虚拟设备: MT6768 (V5 clean)  hw=0x0707  da=0x6768  proto=V5  safety=0x0
[03:50:41.031] [INFO ] [sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）
[plan] goal=printgpt mode=auto
[plan] device: chip=MT6768 (V5 clean)(0x6768) proto=V5(XFlash) safety=[] brom_reachable=true
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] da_source=sim-injected
[plan] selected (cost=690): da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本最低（690 vs 840，步数 3 vs 4）；无安全标志，直接加载 DA 最省步骤
[plan] alternative #2 (cost=840): exploit(kamakiri2) -> da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[03:50:41.031] [INFO ] [exec] step 1/3: da_load(V5(XFlash))
[03:50:41.031] [INFO ] [exec] step 2/3: param_adjust(open_storage)
[03:50:41.031] [INFO ] [exec] step 3/3: param_adjust(parse_gpt)
[1m[32m| 共 10 个分区                                                                                              |[0m[0m
[03:50:41.034] [INFO ] [exec] 完成: GPT 已解析，8 个分区
[03:50:41.034] [INFO ] [sim] 执行完成：3 步，cost=690
```

## $ mtkclient-rs --sim v5-sla printgpt
```
[sim] 虚拟设备: MT6768 (SLA enabled)  hw=0x0707  da=0x6768  proto=V5  safety=0x2
[03:50:41.958] [INFO ] [sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）
[plan] goal=printgpt mode=auto
[plan] device: chip=MT6768 (SLA enabled)(0x6768) proto=V5(XFlash) safety=[sla] brom_reachable=true
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] da_source=sim-injected
[plan] selected (cost=840): exploit(kamakiri2) -> da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本并列（840），按 BROM 优先/步骤更少打破平局；安全标志要求先 bypass，因此选择带 exploit 的路径
[plan] alternative #2 (cost=840): da_load(V5(XFlash)) -> exploit(carbonara) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[03:50:41.959] [INFO ] [exec] step 1/4: exploit(kamakiri2)
[03:50:41.959] [INFO ] [exec] step 2/4: da_load(V5(XFlash))
[03:50:41.959] [INFO ] [exec] step 3/4: param_adjust(open_storage)
[03:50:41.959] [INFO ] [exec] step 4/4: param_adjust(parse_gpt)
[1m[32m| 共 10 个分区                                                                                              |[0m[0m
[03:50:41.961] [INFO ] [exec] 完成: GPT 已解析，8 个分区
[03:50:41.961] [INFO ] [sim] 执行完成：4 步，cost=840
```

## $ mtkclient-rs --sim v6 printgpt
```
[sim] 虚拟设备: MT6983 (V6 XML)  hw=0x0A0A  da=0x6983  proto=V6  safety=0x0
[03:50:43.504] [INFO ] [sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）
[plan] goal=printgpt mode=auto
[plan] device: chip=MT6983 (V6 XML)(0x6983) proto=V6(XML) safety=[] brom_reachable=false
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] da_source=sim-injected
[plan] selected (cost=690): da_load(V6(XML)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 唯一可行路径；V6 芯片 BROM 不可达，直接走 DA 加载
[03:50:43.504] [INFO ] [exec] step 1/3: da_load(V6(XML))
[03:50:43.504] [INFO ] [exec] step 2/3: param_adjust(open_storage)
[03:50:43.504] [INFO ] [exec] step 3/3: param_adjust(parse_gpt)
[1m[32m| 共 8 个分区                                                                                               |[0m[0m
[03:50:43.506] [INFO ] [exec] 完成: GPT 已解析，6 个分区
[03:50:43.506] [INFO ] [sim] 执行完成：3 步，cost=690
```

## $ mtkclient-rs --sim ufs-ab printgpt
```
[sim] 虚拟设备: MT6877 (UFS, A/B)  hw=0x0707  da=0x6877  proto=V5  safety=0x0
[03:50:44.417] [INFO ] [sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）
[plan] goal=printgpt mode=auto
[plan] device: chip=MT6877 (UFS, A/B)(0x6877) proto=V5(XFlash) safety=[] brom_reachable=true
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] da_source=sim-injected
[plan] selected (cost=690): da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本最低（690 vs 840，步数 3 vs 4）；无安全标志，直接加载 DA 最省步骤
[plan] alternative #2 (cost=840): exploit(kamakiri2) -> da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[03:50:44.417] [INFO ] [exec] step 1/3: da_load(V5(XFlash))
[03:50:44.417] [INFO ] [exec] step 2/3: param_adjust(open_storage)
[03:50:44.417] [INFO ] [exec] step 3/3: param_adjust(parse_gpt)
[1m[32m| 共 7 个分区                                                                                               |[0m[0m
[03:50:44.419] [INFO ] [exec] 完成: GPT 已解析，7 个分区
[03:50:44.419] [INFO ] [sim] 执行完成：3 步，cost=690
```

## $ mtkclient-rs --sim no-emmc-info printgpt
```
[sim] 虚拟设备: MT6768 (no emmc info)  hw=0x0707  da=0x6768  proto=V5  safety=0x0
[03:50:45.309] [INFO ] [sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）
[plan] goal=printgpt mode=auto
[plan] device: chip=MT6768 (no emmc info)(0x6768) proto=V5(XFlash) safety=[] brom_reachable=true
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] da_source=sim-injected
[plan] selected (cost=690): da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本最低（690 vs 840，步数 3 vs 4）；无安全标志，直接加载 DA 最省步骤
[plan] alternative #2 (cost=840): exploit(kamakiri2) -> da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[03:50:45.309] [INFO ] [exec] step 1/3: da_load(V5(XFlash))
[03:50:45.309] [INFO ] [exec] step 2/3: param_adjust(open_storage)
[03:50:45.309] [WARN ] [engine] 存储信息查询失败（继续，容量字段显示为 0）: 协议错误: EMMC_INFO 状态 0xC0010004
[03:50:45.309] [INFO ] [exec] step 3/3: param_adjust(parse_gpt)
[1m[32m| 共 8 个分区                                                                                               |[0m[0m
[03:50:45.311] [INFO ] [exec] 完成: GPT 已解析，8 个分区
[03:50:45.311] [INFO ] [sim] 执行完成：3 步，cost=690
```

## $ mtkclient-rs --sim flaky printgpt
```
[sim] 虚拟设备: MT6768 (flaky link)  hw=0x0707  da=0x6768  proto=V5  safety=0x0
[03:50:46.169] [INFO ] [sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）
[plan] goal=printgpt mode=auto
[plan] device: chip=MT6768 (flaky link)(0x6768) proto=V5(XFlash) safety=[] brom_reachable=true
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] da_source=sim-injected
[plan] selected (cost=690): da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本最低（690 vs 840，步数 3 vs 4）；无安全标志，直接加载 DA 最省步骤
[plan] alternative #2 (cost=840): exploit(kamakiri2) -> da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[03:50:46.169] [INFO ] [exec] step 1/3: da_load(V5(XFlash))
[1m[31m[sim] 失败: 步骤 da_load(V5(XFlash)) 失败: 端口错误: 传输超时
[sim] 设备事件:
[sim] 设备侧事件:
```

## $ mtkclient-rs --sim unplug printgpt
```
[sim] 虚拟设备: MT6768 (unplugged)  hw=0x0707  da=0x6768  proto=V5  safety=0x0
[03:50:47.017] [INFO ] [sim] 未提供 --da，使用内置合成 DA（仅用于仿真流程）
[plan] goal=printgpt mode=auto
[plan] device: chip=MT6768 (unplugged)(0x6768) proto=V5(XFlash) safety=[] brom_reachable=true
[plan] required=[storage_read,gpt_parsed] current=[] gap=[storage_read,gpt_parsed]
[plan] da_source=sim-injected
[plan] selected (cost=690): da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[plan] reason: 成本最低（690 vs 840，步数 3 vs 4）；无安全标志，直接加载 DA 最省步骤
[plan] alternative #2 (cost=840): exploit(kamakiri2) -> da_load(V5(XFlash)) -> param_adjust(open_storage) -> param_adjust(parse_gpt)
[03:50:47.017] [INFO ] [exec] step 1/3: da_load(V5(XFlash))
[1m[31m[sim] 失败: 步骤 da_load(V5(XFlash)) 失败: 端口错误: 设备已断开
[sim] 设备事件:
[sim] 设备侧事件:
```

