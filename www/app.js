(function () {
  "use strict";

  const state = {
    scope: "host",
    period: "-12h",
    end: "now",
    customLabel: null,
    search: "",
    cpuStepped: localStorage.getItem("mrtg_cpu_stepped") !== "false",
    targets: [],
    charts: new Map(),
    cardElements: new Map(),
    modalChart: null,
    modalCurrentItem: null,
    modalAbortController: null,
    activeAbortController: null,
    theme: localStorage.getItem("mrtg_theme") || (window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light"),
  };

  const el = {
    tabHost: document.getElementById("tab-host"),
    tabDocker: document.getElementById("tab-docker"),
    searchInput: document.getElementById("search-input"),
    periodBtns: document.querySelectorAll(".period-btn[data-period]"),
    morePeriodsSelect: document.getElementById("more-periods-select"),
    customRangeBtn: document.getElementById("custom-range-btn"),
    customRangeModal: document.getElementById("custom-range-modal"),
    customRangeClose: document.getElementById("custom-range-close"),
    customRangeCancel: document.getElementById("custom-range-cancel"),
    customRangeBackdrop: document.getElementById("custom-range-backdrop"),
    customRangeForm: document.getElementById("custom-range-form"),
    customStart: document.getElementById("custom-start"),
    customEnd: document.getElementById("custom-end"),
    activeRangeBadge: document.getElementById("active-range-badge"),
    cpuScaleToggle: document.getElementById("cpu-scale-toggle"),
    themeToggle: document.getElementById("theme-toggle"),
    targetCount: document.getElementById("target-count"),
    loadingIndicator: document.getElementById("loading-indicator"),
    gridContainer: document.getElementById("grid-container"),
    emptyState: document.getElementById("empty-state"),
    modal: document.getElementById("modal-container"),
    modalClose: document.getElementById("modal-close"),
    modalBackdrop: document.getElementById("modal-backdrop"),
    modalTitle: document.getElementById("modal-title"),
    modalStats: document.getElementById("modal-stats"),
    modalChart: document.getElementById("modal-chart"),
  };

  function updateScaleButton() {
    if (!el.cpuScaleToggle) return;
    const btnText = el.cpuScaleToggle.querySelector(".btn-text");
    if (state.cpuStepped) {
      el.cpuScaleToggle.classList.add("active");
      el.cpuScaleToggle.setAttribute("aria-pressed", "true");
      if (btnText) btnText.textContent = "CPU 100%";
      el.cpuScaleToggle.title = "Escala de CPU: Degraus de 100% (fixa). Clique para alternar para escala automática.";
    } else {
      el.cpuScaleToggle.classList.remove("active");
      el.cpuScaleToggle.setAttribute("aria-pressed", "false");
      if (btnText) btnText.textContent = "CPU Auto";
      el.cpuScaleToggle.title = "Escala de CPU: Automática (detalhe em baixo uso). Clique para alternar para degraus de 100%.";
    }
  }

  function updateActiveRangeBadge() {
    if (!el.activeRangeBadge) return;
    if (state.customLabel) {
      el.activeRangeBadge.textContent = state.customLabel;
    } else if (typeof state.period === "string" && state.period.startsWith("-")) {
      el.activeRangeBadge.textContent = state.period.slice(1);
    } else {
      el.activeRangeBadge.textContent = "Personalizado";
    }
  }

  function setPeriod(startVal, endVal = "now", label = null) {
    state.period = startVal;
    state.end = endVal;
    state.customLabel = label;

    el.periodBtns.forEach((b) => {
      const p = b.getAttribute("data-period");
      if (p === startVal && endVal === "now") {
        b.classList.add("active");
      } else {
        b.classList.remove("active");
      }
    });

    if (el.morePeriodsSelect) {
      if (endVal === "now" && Array.from(el.morePeriodsSelect.options).some((o) => o.value === startVal)) {
        el.morePeriodsSelect.value = startVal;
        el.morePeriodsSelect.classList.add("active");
      } else {
        el.morePeriodsSelect.value = "";
        el.morePeriodsSelect.classList.remove("active");
      }
    }

    if (el.customRangeBtn) {
      if (endVal !== "now" || label) {
        el.customRangeBtn.classList.add("active");
      } else {
        el.customRangeBtn.classList.remove("active");
      }
    }

    updateActiveRangeBadge();
    renderGrid();
    if (state.modalCurrentItem && !el.modal.classList.contains("hidden")) {
      openModal(state.modalCurrentItem);
    }
  }

  // Set initial theme
  function applyTheme(theme) {
    state.theme = theme;
    if (theme === "dark") {
      document.documentElement.setAttribute("data-theme", "dark");
      el.themeToggle.querySelector(".theme-icon").textContent = "☀️";
    } else {
      document.documentElement.setAttribute("data-theme", "light");
      el.themeToggle.querySelector(".theme-icon").textContent = "🌙";
    }
    localStorage.setItem("mrtg_theme", theme);
    // Re-render open charts with updated colors
    state.charts.forEach((chart) => chart.resize());
    if (state.modalChart) state.modalChart.resize();
  }

  applyTheme(state.theme);

  // Formatting helpers
  function formatValue(val, targetId) {
    if (val === null || val === undefined || isNaN(val)) return "-";
    if (targetId.startsWith("docker_") && targetId.endsWith("_*")) return val.toFixed(1) + "%";
    if (targetId.startsWith("temp_")) return val.toFixed(1) + " °C";
    if (targetId.startsWith("smart_")) return Math.round(val).toString();
    if (targetId === "uptime") return Math.floor(val / 60).toLocaleString("pt-BR") + " min";
    if (targetId.includes("cpu") || targetId === "loadavg") {
      return val.toFixed(2);
    }
    if (targetId.includes("mem") || targetId.includes("disk") || targetId.includes("network")) {
      const units = ["B", "KiB", "MiB", "GiB", "TiB"];
      let u = 0;
      let v = val;
      while (v >= 1024 && u < units.length - 1) {
        v /= 1024;
        u++;
      }
      return `${v.toFixed(1)} ${units[u]}`;
    }
    if (val >= 1000000) return (val / 1000000).toFixed(1) + "M";
    if (val >= 1000) return (val / 1000).toFixed(1) + "k";
    return val.toFixed(1);
  }

  function formatYAxis(val, targetId) {
    if (val === null || val === undefined || isNaN(val)) return "0";
    if (targetId.startsWith("docker_") && targetId.endsWith("_*")) return Math.round(val) + "%";
    if (targetId.startsWith("temp_")) return Math.round(val) + " °C";
    if (targetId.startsWith("smart_")) return Math.round(val).toString();
    if (targetId === "uptime") {
      const min = val / 60;
      return min >= 1000 ? Math.round(min / 1000) + "k min" : Math.round(min) + " min";
    }
    if (targetId.includes("cpu") || targetId === "loadavg") {
      return Math.round(val).toString();
    }
    if (targetId.includes("mem") || targetId.includes("disk") || targetId.includes("network")) {
      const units = ["B", "KiB", "MiB", "GiB", "TiB"];
      let u = 0;
      let v = val;
      while (v >= 1024 && u < units.length - 1) {
        v /= 1024;
        u++;
      }
      return `${Math.round(v)} ${units[u]}`;
    }
    if (val >= 1000000) return Math.round(val / 1000000) + "M";
    if (val >= 1000) return Math.round(val / 1000) + "k";
    return Math.round(val).toString();
  }

  const DS_LABELS = {
    loadavg: { ds0: "1 min", ds1: "15 min" },
    cpu_user_idle: { ds0: "User", ds1: "Idle" },
    cpu_iowait_steal: { ds0: "I/O Wait", ds1: "Steal" },
    cpu_system_other: { ds0: "System", ds1: "Other" },
    mem_swap: { ds0: "RAM", ds1: "Swap" },
    disk_io: { ds0: "Reads", ds1: "Writes" },
    disk_usage: { ds0: "Usado", ds1: "Livre" },
    network: { ds0: "Inbound", ds1: "Outbound" },
    uptime: { ds0: "Uptime" },
  };

  function getDsLabel(target, ds) {
    if (target.startsWith("smart_")) {
      if (ds === "ds0") return "Realocados / erros de mídia";
      if (ds === "ds1") return "Pendentes / aviso crítico";
    }
    if (DS_LABELS[target] && DS_LABELS[target][ds]) {
      return DS_LABELS[target][ds];
    }
    if (target.startsWith("docker_")) {
      if (target.endsWith("_cpu")) return "CPU";
      if (target.endsWith("_mem")) return ds === "ds0" ? "Uso" : "Limite";
    }
    return ds.toUpperCase();
  }

  function palette(isDark) {
    return isDark
      ? ["#38bdf8", "#4ade80", "#f472b6", "#facc15", "#a78bfa", "#fb923c", "#2dd4bf", "#f87171"]
      : ["#0284c7", "#16a34a", "#db2777", "#ca8a04", "#7c3aed", "#ea580c", "#0d9488", "#dc2626"];
  }

  // Host targets merged into a single chart, one line per sensor or data source.
  // Virtual target ids contain "*", which valid_target() rejects, so they never
  // collide with real RRDs.
  const GROUPS = [
    { target: "temp_*", name: "Temperaturas", match: (t) => t.startsWith("temp_") },
    { target: "smart_*", name: "SMART", match: (t) => t.startsWith("smart_") },
    {
      target: "cpu_*",
      name: "CPU I/O Wait / Steal / System / Other",
      match: (t) => t === "cpu_iowait_steal" || t === "cpu_system_other",
    },
  ];

  // Docker scope: docker_<name>_cpu and docker_<name>_mem share one percent chart.
  function groupOf(t) {
    if (t.scope === "host") return GROUPS.find((g) => g.match(t.target));
    const m = /^docker_(.+)_(cpu|mem)$/.exec(t.target);
    return m && { target: `docker_${m[1]}_*`, name: m[1], pct: true };
  }

  function groupTargets(targets) {
    const out = [];
    const groups = new Map();
    for (const t of targets) {
      const g = groupOf(t);
      if (!g) {
        out.push(t);
        continue;
      }
      let item = groups.get(g.target);
      if (!item) {
        item = { scope: t.scope, target: g.target, name: g.name, pct: g.pct, members: [] };
        groups.set(g.target, item);
        out.push(item);
      }
      item.members.push(t);
    }
    return out;
  }

  function combineGroup(item, results) {
    const parts = [];
    for (const m of item.members) {
      const r = results[m.target];
      if (!r) continue;
      if (m.target.endsWith("_mem") && item.pct) {
        // Memory as percent of its limit (ds1, memory.max or host MemTotal).
        const values = r.series.ds0.map((used, i) => {
          const limit = r.series.ds1 && r.series.ds1[i];
          return used != null && limit ? (used / limit) * 100 : null;
        });
        const known = values.filter((v) => v != null);
        parts.push({
          key: `${m.target}:pct`,
          label: "Memória (% do limite)",
          timestamps: r.timestamps,
          values,
          stats: {
            max: known.length ? Math.max(...known) : null,
            current: known.length ? known[known.length - 1] : null,
          },
        });
        continue;
      }
      const label = m.name.replace(/^(Temperatura|SMART) /, "");
      // The API returns only ds0 for temp_* and docker *_cpu targets.
      for (const ds of Object.keys(r.series)) {
        parts.push({
          key: `${m.target}:${ds}`,
          label: m.target.startsWith("smart_") ? `${label} ${getDsLabel(m.target, ds)}`
            : m.target.startsWith("cpu_") || item.pct ? getDsLabel(m.target, ds) : label,
          timestamps: r.timestamps,
          values: r.series[ds],
          stats: r.stats[ds],
        });
      }
    }
    if (!parts.length) return null;
    const timestamps = [...new Set(parts.flatMap((p) => p.timestamps))].sort((a, b) => a - b);
    const index = new Map(timestamps.map((t, i) => [t, i]));
    const data = { timestamps, series: {}, stats: {}, labels: {} };
    for (const p of parts) {
      const values = new Array(timestamps.length).fill(null);
      p.timestamps.forEach((t, i) => {
        values[index.get(t)] = p.values[i];
      });
      data.series[p.key] = values;
      data.stats[p.key] = p.stats;
      data.labels[p.key] = p.label;
    }
    return data;
  }

  // query-batch accepts at most 32 targets per request.
  async function queryTargets(scope, targets, signal) {
    const results = {};
    for (let i = 0; i < targets.length; i += 32) {
      const targetsQuery = targets.slice(i, i + 32).map((t) => `target=${encodeURIComponent(t)}`).join("&");
      const url = `/api/query-batch?scope=${encodeURIComponent(scope)}&start=${encodeURIComponent(state.period)}&end=${encodeURIComponent(state.end || "now")}&${targetsQuery}`;
      const resp = await fetch(url, { signal });
      if (!resp.ok) throw new Error("Erro na consulta em lote");
      Object.assign(results, (await resp.json()).results || {});
    }
    return results;
  }

  function formatTimestamp(ts, timestamps) {
    const d = new Date(ts * 1000);
    let spanSec = 86400;
    if (timestamps && timestamps.length >= 2) {
      spanSec = timestamps[timestamps.length - 1] - timestamps[0];
    } else if (typeof state.period === "string" && state.period.startsWith("-")) {
      const unit = state.period.slice(-1);
      const num = parseInt(state.period.slice(1, -1), 10) || 24;
      if (unit === "m") spanSec = num * 60;
      else if (unit === "h") spanSec = num * 3600;
      else if (unit === "d") spanSec = num * 86400;
      else if (unit === "w") spanSec = num * 604800;
      else if (unit === "y") spanSec = num * 31536000;
    }

    if (spanSec <= 86400) {
      return d.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
    }
    if (spanSec <= 2592000) {
      return d.toLocaleDateString("pt-BR", { month: "numeric", day: "numeric" }) + " " +
             d.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
    }
    return d.toLocaleDateString("pt-BR", { year: "numeric", month: "numeric", day: "numeric" });
  }

  // Queue runner for max 6 concurrent queries
  class FetchQueue {
    constructor(maxConcurrent = 6) {
      this.maxConcurrent = maxConcurrent;
      this.active = 0;
      this.queue = [];
    }

    push(task) {
      return new Promise((resolve, reject) => {
        this.queue.push({ task, resolve, reject });
        this.next();
      });
    }

    next() {
      if (this.active >= this.maxConcurrent || this.queue.length === 0) return;
      this.active++;
      const { task, resolve, reject } = this.queue.shift();
      task()
        .then(resolve)
        .catch(reject)
        .finally(() => {
          this.active--;
          this.next();
        });
    }

    clear() {
      this.queue = [];
    }
  }

  const fetchQueue = new FetchQueue(6);

  async function loadTargets() {
    el.loadingIndicator.classList.remove("hidden");
    try {
      const resp = await fetch("/api/targets");
      if (!resp.ok) throw new Error("Erro ao carregar lista de métricas");
      state.targets = groupTargets(await resp.json());
      renderGrid();
    } catch (err) {
      el.targetCount.textContent = "Erro ao conectar com API";
    } finally {
      el.loadingIndicator.classList.add("hidden");
    }
  }

  function cardKey(item) {
    return `${item.scope}:${item.target}`;
  }

  function buildCard(item) {
    const card = document.createElement("div");
    card.className = "metric-card";
    card.setAttribute("role", "button");
    card.setAttribute("tabindex", "0");
    card.setAttribute("aria-label", `Ver gráfico detalhado de ${item.name}`);

    const header = document.createElement("div");
    header.className = "card-header";

    const title = document.createElement("div");
    title.className = "card-title";
    title.textContent = item.name;

    const badge = document.createElement("span");
    badge.className = "card-badge";
    badge.textContent = item.scope;

    header.appendChild(title);
    header.appendChild(badge);

    const statsRow = document.createElement("div");
    statsRow.className = "stats-row";

    const maxStat = createStatItem("Max", "-");
    const avgStat = createStatItem("Média", "-");
    const curStat = createStatItem("Atual", "-");

    statsRow.appendChild(maxStat.container);
    if (!item.members) statsRow.appendChild(avgStat.container);
    statsRow.appendChild(curStat.container);

    const chartContainer = document.createElement("div");
    chartContainer.className = "chart-container";

    card.appendChild(header);
    card.appendChild(statsRow);
    card.appendChild(chartContainer);

    card.addEventListener("click", () => openModal(item));
    card.addEventListener("keydown", (e) => {
      if (e.key === "Enter" || e.key === " ") {
        e.preventDefault();
        openModal(item);
      }
    });

    return { card, chartContainer, statRefs: { maxStat, avgStat, curStat } };
  }

  const BATCH_SIZE = 8;

  function renderGrid() {
    // Abort pending fetches
    if (state.activeAbortController) {
      state.activeAbortController.abort();
    }
    state.activeAbortController = new AbortController();
    fetchQueue.clear();

    const filtered = state.targets.filter((t) => {
      if (t.scope !== state.scope) return false;
      if (!state.search) return true;
      const q = state.search.toLowerCase();
      return t.name.toLowerCase().includes(q) || t.target.toLowerCase().includes(q) ||
        (t.members || []).some((m) => m.name.toLowerCase().includes(q) || m.target.toLowerCase().includes(q));
    });

    const liveKeys = new Set(filtered.map(cardKey));
    for (const [key, info] of state.cardElements.entries()) {
      if (!liveKeys.has(key)) {
        info.card.remove();
        const chart = state.charts.get(key);
        if (chart) {
          chart.dispose();
          state.charts.delete(key);
        }
        state.cardElements.delete(key);
      }
    }

    el.targetCount.textContent = `${filtered.length} métrica${filtered.length === 1 ? "" : "s"} (${state.scope === "host" ? "Host" : "Docker"})`;

    if (filtered.length === 0) {
      el.emptyState.classList.remove("hidden");
      return;
    }
    el.emptyState.classList.add("hidden");

    const signal = state.activeAbortController.signal;
    const refs = new Map();
    filtered.forEach((item) => {
      const key = cardKey(item);
      let info = state.cardElements.get(key);
      if (!info) {
        info = buildCard(item);
        state.cardElements.set(key, info);
      }
      el.gridContainer.appendChild(info.card);
      refs.set(key, { container: info.chartContainer, statRefs: info.statRefs });
    });

    for (let i = 0; i < filtered.length; i += BATCH_SIZE) {
      const chunk = filtered.slice(i, i + BATCH_SIZE);
      fetchQueue.push(() => fetchAndRenderBatch(state.scope, chunk, refs, signal));
    }
  }

  function createStatItem(label, initialVal) {
    const container = document.createElement("div");
    container.className = "stat-item";
    const lbl = document.createElement("span");
    lbl.className = "stat-label";
    lbl.textContent = label;
    const val = document.createElement("span");
    val.className = "stat-value";
    val.textContent = initialVal;
    container.appendChild(lbl);
    container.appendChild(val);
    return { container, val };
  }

  function renderCard(item, container, statRefs, data) {
    // Update stats from first DS (or highest)
    const dsKeys = Object.keys(data.stats);
    if (item.members) {
      // Grouped chart: hottest sensor / highest SMART counter across all lines.
      const pick = (field) => {
        const v = Math.max(...dsKeys.map((ds) => data.stats[ds][field] ?? -Infinity));
        return Number.isFinite(v) ? v : null;
      };
      statRefs.maxStat.val.textContent = formatValue(pick("max"), item.target);
      statRefs.curStat.val.textContent = formatValue(pick("current"), item.target);
    } else if (dsKeys.length > 0) {
      const firstDs = data.stats[dsKeys[0]];
      statRefs.maxStat.val.textContent = formatValue(firstDs.max, item.target);
      statRefs.avgStat.val.textContent = formatValue(firstDs.avg, item.target);
      statRefs.curStat.val.textContent = formatValue(firstDs.current, item.target);
    }

    // Initialize ECharts on container
    const isDark = state.theme === "dark";
    const key = cardKey(item);
    let chart = state.charts.get(key);
    if (!chart) {
      container.innerHTML = "";
      chart = echarts.init(container, null, { renderer: "canvas" });
      state.charts.set(key, chart);
    }

    const seriesList = [];
    const colors = palette(isDark);

    const isCpuTarget = state.cpuStepped && (item.target.includes("cpu") || item.pct);
    let peak = 0;
    if (data.stats) {
      Object.values(data.stats).forEach((s) => {
        if (s && s.max != null && s.max > peak) peak = s.max;
      });
    }
    const steppedMax = Math.max(100, Math.ceil(peak / 100) * 100);

    dsKeys.forEach((ds, idx) => {
      const sObj = {
        name: data.labels ? data.labels[ds] : getDsLabel(item.target, ds),
        type: "line",
        showSymbol: false,
        connectNulls: true,
        smooth: true,
        lineStyle: { width: 1.5, color: colors[idx % colors.length] },
        areaStyle: idx === 0 ? {
          opacity: 0.15,
          color: new echarts.graphic.LinearGradient(0, 0, 0, 1, [
            { offset: 0, color: colors[idx % colors.length] },
            { offset: 1, color: isDark ? "rgba(0,0,0,0)" : "rgba(255,255,255,0)" },
          ]),
        } : undefined,
        data: data.series[ds],
      };

      if (isCpuTarget && idx === 0 && steppedMax > 100) {
        sObj.markLine = {
          symbol: "none",
          silent: true,
          data: [
            {
              yAxis: 100,
              lineStyle: { color: "#ef4444", type: "dashed", width: 1.5 },
              label: {
                show: true,
                formatter: "100%",
                position: "insideEndTop",
                color: "#ef4444",
                fontSize: 9,
              },
            },
          ],
        };
      }

      seriesList.push(sObj);
    });

    const yAxisConfig = {
      type: "value",
      minInterval: 1,
      splitLine: { lineStyle: { color: isDark ? "#1e293b" : "#f1f5f9" } },
      axisLabel: {
        color: isDark ? "#94a3b8" : "#64748b",
        fontSize: 10,
        formatter: (v) => formatYAxis(v, item.target),
      },
    };

    if (isCpuTarget) {
      yAxisConfig.min = 0;
      yAxisConfig.max = steppedMax;
      yAxisConfig.interval = steppedMax <= 100 ? 50 : 100;
    }

    const option = {
      animation: false,
      grid: { top: isCpuTarget && steppedMax > 100 ? 15 : 10, right: 10, bottom: 25, left: 35 },
      tooltip: {
        trigger: "axis",
        formatter: (params) => {
          if (!params || !params.length) return "";
          const ts = data.timestamps[params[0].dataIndex];
          let out = `${formatTimestamp(ts, data.timestamps)}<br/>`;
          params.forEach((p) => {
            out += `${p.marker} ${p.seriesName}: <b>${formatValue(p.value, item.target)}</b><br/>`;
          });
          return out;
        },
      },
      dataZoom: [
        {
          type: "inside",
          zoomOnMouseWheel: true,
          moveOnMouseMove: true,
          moveOnMouseWheel: true,
        },
      ],
      xAxis: {
        type: "category",
        data: data.timestamps.map((t) => formatTimestamp(t, data.timestamps)),
        axisLine: { lineStyle: { color: isDark ? "#334155" : "#cbd5e1" } },
        axisLabel: { color: isDark ? "#94a3b8" : "#64748b", fontSize: 10, interval: "auto" },
      },
      yAxis: yAxisConfig,
      series: seriesList,
    };

    // replaceMerge: sem isso o merge mantém min/max/interval e markLine antigos
    // ao alternar a escala de CPU (instância é reciclada, não recriada).
    // Não usar notMerge: true — perderia o estado do dataZoom.
    chart.setOption(option, { replaceMerge: ["series", "yAxis"] });
  }

  function renderCardError(item, container) {
    const key = cardKey(item);
    const stale = state.charts.get(key);
    if (stale) {
      stale.dispose();
      state.charts.delete(key);
    }
    container.innerHTML = "";
    const errBox = document.createElement("div");
    errBox.className = "chart-error";
    errBox.textContent = "Sem dados disponíveis";
    container.appendChild(errBox);
  }

  async function fetchAndRenderBatch(scope, items, refs, signal) {
    try {
      const results = await queryTargets(scope, items.flatMap((i) => (i.members ? i.members.map((m) => m.target) : [i.target])), signal);

      for (const item of items) {
        const ref = refs.get(cardKey(item));
        if (!ref) continue;
        const result = item.members ? combineGroup(item, results) : results[item.target];
        if (result) {
          renderCard(item, ref.container, ref.statRefs, result);
        } else {
          renderCardError(item, ref.container);
        }
      }
    } catch (err) {
      if (err.name === "AbortError") return;
      for (const item of items) {
        const ref = refs.get(cardKey(item));
        if (ref) renderCardError(item, ref.container);
      }
    }
  }

  async function openModal(item) {
    if (state.modalAbortController) {
      state.modalAbortController.abort();
    }
    state.modalAbortController = new AbortController();
    const signal = state.modalAbortController.signal;

    state.modalCurrentItem = item;
    el.modalTitle.textContent = item.name;
    while (el.modalStats.firstChild) {
      el.modalStats.removeChild(el.modalStats.firstChild);
    }
    el.modal.classList.remove("hidden");

    if (state.modalChart) {
      state.modalChart.dispose();
      state.modalChart = null;
    }

    try {
      let data;
      if (item.members) {
        data = combineGroup(item, await queryTargets(item.scope, item.members.map((m) => m.target), signal));
        if (!data) throw new Error("Erro na consulta");
      } else {
        const url = `/api/query?scope=${encodeURIComponent(item.scope)}&target=${encodeURIComponent(item.target)}&start=${encodeURIComponent(state.period)}&end=${encodeURIComponent(state.end || "now")}`;
        const resp = await fetch(url, { signal });
        if (!resp.ok) throw new Error("Erro na consulta");
        data = await resp.json();
      }

      const dsKeys = Object.keys(data.stats);
      dsKeys.forEach((ds) => {
        const dsStat = data.stats[ds];
        const box = document.createElement("div");
        box.style.display = "flex";
        box.style.gap = "1rem";
        box.style.fontSize = "0.85rem";

        const dsTitle = document.createElement("span");
        dsTitle.style.fontWeight = "700";
        dsTitle.textContent = `${data.labels ? data.labels[ds] : getDsLabel(item.target, ds)}:`;

        const maxSpan = document.createElement("span");
        maxSpan.textContent = `Max: ${formatValue(dsStat.max, item.target)}`;

        const avgSpan = document.createElement("span");
        avgSpan.textContent = `Média: ${formatValue(dsStat.avg, item.target)}`;

        const curSpan = document.createElement("span");
        curSpan.textContent = `Atual: ${formatValue(dsStat.current, item.target)}`;

        box.appendChild(dsTitle);
        box.appendChild(maxSpan);
        if (!item.members) box.appendChild(avgSpan);
        box.appendChild(curSpan);
        el.modalStats.appendChild(box);
      });

      const isDark = state.theme === "dark";
      state.modalChart = echarts.init(el.modalChart, null, { renderer: "canvas" });

      const colors = palette(isDark);
      const isCpuTarget = state.cpuStepped && (item.target.includes("cpu") || item.pct);
      let peak = 0;
      if (data.stats) {
        Object.values(data.stats).forEach((s) => {
          if (s && s.max != null && s.max > peak) peak = s.max;
        });
      }
      const steppedMax = Math.max(100, Math.ceil(peak / 100) * 100);

      const seriesList = dsKeys.map((ds, idx) => {
        const sObj = {
          name: data.labels ? data.labels[ds] : getDsLabel(item.target, ds),
          type: "line",
          showSymbol: false,
          connectNulls: true,
          smooth: true,
          lineStyle: { width: 2, color: colors[idx % colors.length] },
          data: data.series[ds],
        };

        if (isCpuTarget && idx === 0 && steppedMax > 100) {
          sObj.markLine = {
            symbol: "none",
            silent: true,
            data: [
              {
                yAxis: 100,
                lineStyle: { color: "#ef4444", type: "dashed", width: 2 },
                label: {
                  show: true,
                  formatter: "100% (1 core)",
                  position: "insideEndTop",
                  color: "#ef4444",
                  fontSize: 11,
                  fontWeight: "bold",
                },
              },
            ],
          };
        }

        return sObj;
      });

      const yAxisConfig = {
        type: "value",
        minInterval: 1,
        axisLabel: {
          color: isDark ? "#94a3b8" : "#64748b",
          formatter: (v) => formatYAxis(v, item.target),
        },
        splitLine: { lineStyle: { color: isDark ? "#1e293b" : "#f1f5f9" } },
      };

      if (isCpuTarget) {
        yAxisConfig.min = 0;
        yAxisConfig.max = steppedMax;
        yAxisConfig.interval = steppedMax <= 100 ? 50 : 100;
      }

      const option = {
        grid: { top: 30, right: 20, bottom: 45, left: 60 },
        tooltip: {
          trigger: "axis",
          formatter: (params) => {
            if (!params || !params.length) return "";
            const ts = data.timestamps[params[0].dataIndex];
            let out = `${formatTimestamp(ts, data.timestamps)}<br/>`;
            params.forEach((p) => {
              out += `${p.marker} ${p.seriesName}: <b>${formatValue(p.value, item.target)}</b><br/>`;
            });
            return out;
          },
        },
        dataZoom: [
          {
            type: "inside",
            zoomOnMouseWheel: true,
            moveOnMouseMove: true,
          },
          {
            type: "slider",
            bottom: 5,
            height: 20,
            borderColor: isDark ? "#334155" : "#cbd5e1",
            textStyle: { color: isDark ? "#94a3b8" : "#64748b", fontSize: 10 },
          },
        ],
        xAxis: {
          type: "category",
          data: data.timestamps.map((t) => formatTimestamp(t, data.timestamps)),
          axisLabel: { color: isDark ? "#94a3b8" : "#64748b" },
        },
        yAxis: yAxisConfig,
        series: seriesList,
      };

      state.modalChart.setOption(option);
    } catch (err) {
      if (err.name === "AbortError") return;
      el.modalStats.textContent = "Erro ao carregar detalhes da métrica.";
    }
  }

  function closeModal() {
    if (state.modalAbortController) {
      state.modalAbortController.abort();
      state.modalAbortController = null;
    }
    el.modal.classList.add("hidden");
    state.modalCurrentItem = null;
    if (state.modalChart) {
      state.modalChart.dispose();
      state.modalChart = null;
    }
  }

  function formatLocalDateInput(dateObj) {
    return new Date(dateObj.getTime() - dateObj.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
  }

  function openCustomRangeModal() {
    const now = new Date();
    const nowLocal = formatLocalDateInput(now);

    let startLocal;
    if (typeof state.period === "string" && state.period.startsWith("-")) {
      const unit = state.period.slice(-1);
      const num = parseInt(state.period.slice(1, -1), 10) || 24;
      let ms = 24 * 3600 * 1000;
      if (unit === "m") ms = num * 60 * 1000;
      else if (unit === "h") ms = num * 3600 * 1000;
      else if (unit === "d") ms = num * 86400 * 1000;
      else if (unit === "w") ms = num * 604800 * 1000;
      else if (unit === "y") ms = num * 31536000 * 1000;
      const startD = new Date(now.getTime() - ms);
      startLocal = formatLocalDateInput(startD);
    } else {
      const yesterday = new Date(now.getTime() - 86400000);
      startLocal = formatLocalDateInput(yesterday);
    }

    if (el.customStart) el.customStart.value = startLocal;
    if (el.customEnd) el.customEnd.value = nowLocal;

    if (el.customRangeModal) el.customRangeModal.classList.remove("hidden");
  }

  function closeCustomRangeModal() {
    if (el.customRangeModal) el.customRangeModal.classList.add("hidden");
  }

  // Event Listeners
  el.tabHost.addEventListener("click", () => {
    if (state.scope === "host") return;
    state.scope = "host";
    el.tabHost.classList.add("active");
    el.tabHost.setAttribute("aria-selected", "true");
    el.tabDocker.classList.remove("active");
    el.tabDocker.setAttribute("aria-selected", "false");
    renderGrid();
  });

  el.tabDocker.addEventListener("click", () => {
    if (state.scope === "docker") return;
    state.scope = "docker";
    el.tabDocker.classList.add("active");
    el.tabDocker.setAttribute("aria-selected", "true");
    el.tabHost.classList.remove("active");
    el.tabHost.setAttribute("aria-selected", "false");
    renderGrid();
  });

  el.periodBtns.forEach((btn) => {
    btn.addEventListener("click", () => {
      const p = btn.getAttribute("data-period");
      if (p) setPeriod(p, "now");
    });
  });

  if (el.morePeriodsSelect) {
    el.morePeriodsSelect.addEventListener("change", (e) => {
      const val = e.target.value;
      if (!val) return;
      setPeriod(val, "now");
    });
  }

  if (el.customRangeBtn) {
    el.customRangeBtn.addEventListener("click", openCustomRangeModal);
  }
  if (el.customRangeClose) {
    el.customRangeClose.addEventListener("click", closeCustomRangeModal);
  }
  if (el.customRangeCancel) {
    el.customRangeCancel.addEventListener("click", closeCustomRangeModal);
  }
  if (el.customRangeBackdrop) {
    el.customRangeBackdrop.addEventListener("click", closeCustomRangeModal);
  }

  document.querySelectorAll(".chip-btn").forEach((chip) => {
    chip.addEventListener("click", () => {
      const now = new Date();
      if (chip.dataset.hours) {
        const h = parseInt(chip.dataset.hours, 10);
        const start = new Date(now.getTime() - h * 3600 * 1000);
        el.customStart.value = formatLocalDateInput(start);
        el.customEnd.value = formatLocalDateInput(now);
      } else if (chip.dataset.days) {
        const d = parseInt(chip.dataset.days, 10);
        const start = new Date(now.getTime() - d * 86400 * 1000);
        el.customStart.value = formatLocalDateInput(start);
        el.customEnd.value = formatLocalDateInput(now);
      } else if (chip.dataset.preset === "today") {
        const start = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 0, 0, 0);
        el.customStart.value = formatLocalDateInput(start);
        el.customEnd.value = formatLocalDateInput(now);
      } else if (chip.dataset.preset === "yesterday") {
        const start = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1, 0, 0, 0);
        const end = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1, 23, 59, 59);
        el.customStart.value = formatLocalDateInput(start);
        el.customEnd.value = formatLocalDateInput(end);
      }
    });
  });

  if (el.customRangeForm) {
    el.customRangeForm.addEventListener("submit", (e) => {
      e.preventDefault();
      const startVal = el.customStart.value;
      const endVal = el.customEnd.value;
      if (!startVal || !endVal) return;

      const startDate = new Date(startVal);
      const endDate = new Date(endVal);
      if (startDate >= endDate) {
        alert("A data inicial deve ser anterior à data final.");
        return;
      }

      const sStr = startDate.toLocaleDateString("pt-BR", { day: "2-digit", month: "2-digit" }) + " " +
                   startDate.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });
      const eStr = endDate.toLocaleDateString("pt-BR", { day: "2-digit", month: "2-digit" }) + " " +
                   endDate.toLocaleTimeString("pt-BR", { hour: "2-digit", minute: "2-digit" });

      closeCustomRangeModal();
      const startEpoch = String(Math.floor(startDate.getTime() / 1000));
      const endEpoch = String(Math.floor(endDate.getTime() / 1000));
      setPeriod(startEpoch, endEpoch, `${sStr} → ${eStr}`);
    });
  }

  if (el.cpuScaleToggle) {
    el.cpuScaleToggle.addEventListener("click", () => {
      state.cpuStepped = !state.cpuStepped;
      localStorage.setItem("mrtg_cpu_stepped", state.cpuStepped);
      updateScaleButton();
      renderGrid();
      if (state.modalCurrentItem && !el.modal.classList.contains("hidden")) {
        openModal(state.modalCurrentItem);
      }
    });
  }

  let searchTimeout = null;
  el.searchInput.addEventListener("input", (e) => {
    clearTimeout(searchTimeout);
    searchTimeout = setTimeout(() => {
      state.search = e.target.value.trim();
      renderGrid();
    }, 250);
  });

  el.themeToggle.addEventListener("click", () => {
    applyTheme(state.theme === "dark" ? "light" : "dark");
  });

  el.modalClose.addEventListener("click", closeModal);
  el.modalBackdrop.addEventListener("click", closeModal);
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      if (el.customRangeModal && !el.customRangeModal.classList.contains("hidden")) {
        closeCustomRangeModal();
      } else if (!el.modal.classList.contains("hidden")) {
        closeModal();
      }
    }
  });

  window.addEventListener("resize", () => {
    state.charts.forEach((chart) => chart.resize());
    if (state.modalChart) state.modalChart.resize();
  });

  // Init
  updateScaleButton();
  updateActiveRangeBadge();

  setInterval(() => {
    if (state.end === "now" && !state.modalCurrentItem && document.visibilityState === "visible") {
      renderGrid();
    }
  }, 60000);

  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "visible" && state.end === "now" && !state.modalCurrentItem) {
      renderGrid();
    }
  });

  loadTargets();
})();
