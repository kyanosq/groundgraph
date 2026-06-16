//! `specslice dashboard` — one-shot self-contained HTML management panel.
//!
//! Aggregates the full analysis surface (overview, business modules, feature
//! clusters, checks, dead code, questions, purity) into a single offline HTML
//! file with zero external resources. Each section is computed independently
//! and degrades to an `{"error": …}` payload, so one failing analysis never
//! blanks the whole panel.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Value};
use specslice_engine::business_pack::{propose_business_pack, BusinessPackOptions};
use specslice_engine::checks::{run_checks, CheckOptions};
use specslice_engine::dead_code::{analyze_dead_code, DeadCodeOptions};
use specslice_engine::feature_map::{analyze_feature_map, FeatureMapOptions};
use specslice_engine::questions::{analyze_questions, QuestionsOptions};
use specslice_engine::symbol_facts::{analyze_symbol_facts, Purity, SymbolFactsOptions};
use specslice_store::Store;

#[derive(Debug, Clone)]
pub struct DashboardRunArgs {
    pub repo_root: PathBuf,
    pub out: Option<PathBuf>,
}

pub fn run(args: DashboardRunArgs) -> Result<()> {
    let repo_root = &args.repo_root;
    let data = collect(repo_root);
    let html = render_dashboard_html(&serde_json::to_string(&data).context("serialising data")?);
    let target = match &args.out {
        Some(p) => p.clone(),
        None => repo_root.join(".specslice/export/dashboard.html"),
    };
    super::output::write_atomic(&target, &html)
        .with_context(|| format!("writing dashboard to {}", target.display()))?;
    eprintln!("wrote {}", target.display());
    eprintln!("open it in any browser — fully offline, no server required.");
    Ok(())
}

/// Run one analysis section, folding errors into a JSON payload instead of
/// aborting the panel. Error chains may carry absolute host paths (store
/// open contexts, IO errors); redact them so a shared dashboard never
/// leaks the host directory layout (issues2.md #40).
fn section<T: serde::Serialize>(repo_root: &Path, result: Result<T>) -> Value {
    match result {
        Ok(v) => serde_json::to_value(v).unwrap_or_else(|e| json!({ "error": e.to_string() })),
        Err(e) => {
            let mut msg = format!("{e:#}");
            let root = repo_root.to_string_lossy();
            if !root.is_empty() {
                msg = msg.replace(root.as_ref(), "<repo>");
            }
            json!({ "error": msg })
        }
    }
}

fn collect(repo_root: &Path) -> Value {
    let overview = section(repo_root, overview(repo_root));
    let modules = section(
        repo_root,
        propose_business_pack(BusinessPackOptions {
            repo_root: repo_root.to_path_buf(),
            ..BusinessPackOptions::default()
        })
        .context("propose"),
    );
    let features = section(
        repo_root,
        analyze_feature_map(FeatureMapOptions {
            repo_root: repo_root.to_path_buf(),
            ..FeatureMapOptions::default()
        })
        .context("features"),
    );
    let checks = section(
        repo_root,
        run_checks(CheckOptions {
            repo_root: repo_root.to_path_buf(),
            impact: None,
        })
        .context("check"),
    );
    let dead_code = section(
        repo_root,
        analyze_dead_code(DeadCodeOptions {
            repo_root: repo_root.to_path_buf(),
            ..DeadCodeOptions::default()
        })
        .context("dead-code"),
    );
    let questions = section(
        repo_root,
        analyze_questions(QuestionsOptions {
            repo_root: repo_root.to_path_buf(),
            ..QuestionsOptions::default()
        })
        .context("questions"),
    );
    let purity = section(repo_root, purity_summary(repo_root));

    // Repo *name* only — dashboards travel (CI artifacts, pasted into
    // issues), and the absolute path would leak usernames and host
    // directory layout (issues2.md #40).
    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repository".to_string());
    json!({
        "meta": {
            "repo": repo_name,
            "generated_at": chrono_lite_now(),
            "tool": format!("specslice {}", env!("CARGO_PKG_VERSION")),
        },
        "overview": overview,
        "modules": modules,
        "features": features,
        "checks": checks,
        "dead_code": dead_code,
        "questions": questions,
        "purity": purity,
    })
}

/// Node / edge counts by kind straight from the store.
fn overview(repo_root: &Path) -> Result<Value> {
    let db = repo_root.join(".specslice").join("graph.db");
    let store =
        Store::open(&db).with_context(|| format!("opening graph store at {}", db.display()))?;
    let nodes = store.list_all_nodes().context("listing nodes")?;
    let edges = store.list_all_edges().context("listing edges")?;
    let mut node_kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for n in &nodes {
        *node_kinds.entry(n.kind.as_str().to_string()).or_default() += 1;
    }
    let mut edge_kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for e in &edges {
        *edge_kinds.entry(e.kind.as_str().to_string()).or_default() += 1;
    }
    Ok(json!({
        "nodes": nodes.len(),
        "edges": edges.len(),
        "node_kinds": node_kinds,
        "edge_kinds": edge_kinds,
    }))
}

/// Purity counts plus a bounded sample of impure symbols (full facts would
/// dominate the payload on large repos).
fn purity_summary(repo_root: &Path) -> Result<Value> {
    let report = analyze_symbol_facts(SymbolFactsOptions {
        repo_root: repo_root.to_path_buf(),
        max_symbols: 0,
        ..SymbolFactsOptions::default()
    })
    .context("purity")?;
    let impure_sample: Vec<Value> = report
        .facts
        .iter()
        .filter(|f| f.purity == Purity::Impure)
        .take(100)
        .map(|f| {
            json!({
                "name": f.name,
                "path": f.path,
                "signals": f.impurity_signals,
            })
        })
        .collect();
    Ok(json!({
        "stats": report.stats,
        "impure_sample": impure_sample,
    }))
}

/// RFC3339-ish local timestamp without pulling a chrono dependency.
fn chrono_lite_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

/// Inline the data into the panel template. Any `</` inside the JSON is
/// escaped to `<\/` (a valid JSON escape) so the payload can never close the
/// host `<script>` tag early.
fn render_dashboard_html(data_json: &str) -> String {
    let safe = data_json.replace("</", "<\\/");
    DASHBOARD_TEMPLATE.replacen(
        "/*SS_DASHBOARD_DATA*/",
        &format!("window.__SS_DASHBOARD__ = {safe};"),
        1,
    )
}

const DASHBOARD_TEMPLATE: &str = r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>SpecSlice Dashboard</title>
<style>
:root{--bg:#0f1115;--panel:#171a21;--panel2:#1d212b;--text:#e6e9f0;--muted:#8b93a7;--accent:#5b8cff;--ok:#3fb96f;--warn:#e0a93e;--err:#e05c5c;--border:#262b38}
*{box-sizing:border-box;margin:0;padding:0}
body{background:var(--bg);color:var(--text);font:14px/1.55 -apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"PingFang SC","Microsoft YaHei",sans-serif}
.layout{display:flex;min-height:100vh}
nav{width:208px;background:var(--panel);border-right:1px solid var(--border);padding:18px 0;position:sticky;top:0;height:100vh}
nav h1{font-size:16px;padding:0 18px 14px;color:var(--accent);letter-spacing:.4px}
nav button{display:block;width:100%;text-align:left;padding:9px 18px;background:none;border:0;color:var(--muted);font-size:14px;cursor:pointer;border-left:3px solid transparent}
nav button:hover{color:var(--text)}
nav button.active{color:var(--text);border-left-color:var(--accent);background:var(--panel2)}
nav .meta{padding:14px 18px;font-size:11px;color:var(--muted);border-top:1px solid var(--border);margin-top:14px;word-break:break-all}
main{flex:1;padding:22px 26px;max-width:1280px}
h2{font-size:18px;margin-bottom:14px}
.cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(170px,1fr));gap:12px;margin-bottom:18px}
.card{background:var(--panel);border:1px solid var(--border);border-radius:10px;padding:14px}
.card .v{font-size:24px;font-weight:600}
.card .k{color:var(--muted);font-size:12px;margin-top:2px}
table{width:100%;border-collapse:collapse;background:var(--panel);border:1px solid var(--border);border-radius:10px;overflow:hidden}
th,td{padding:8px 12px;text-align:left;border-bottom:1px solid var(--border);vertical-align:top}
th{background:var(--panel2);color:var(--muted);font-weight:500;font-size:12px;text-transform:uppercase;letter-spacing:.5px}
tr:last-child td{border-bottom:0}
td.num{text-align:right;font-variant-numeric:tabular-nums}
.tag{display:inline-block;padding:1px 8px;border-radius:99px;font-size:11px;border:1px solid var(--border);color:var(--muted);margin:1px 2px 1px 0}
.tag.ok{color:var(--ok);border-color:var(--ok)}
.tag.warn{color:var(--warn);border-color:var(--warn)}
.tag.err{color:var(--err);border-color:var(--err)}
.muted{color:var(--muted)}
.search{width:100%;max-width:420px;background:var(--panel);border:1px solid var(--border);color:var(--text);border-radius:8px;padding:8px 12px;margin-bottom:14px;font-size:13px}
.search:focus{outline:none;border-color:var(--accent)}
.empty{color:var(--muted);padding:26px;text-align:center;background:var(--panel);border:1px dashed var(--border);border-radius:10px}
.bar{height:8px;border-radius:4px;background:var(--panel2);overflow:hidden;min-width:80px}
.bar i{display:block;height:100%;background:var(--accent)}
code{background:var(--panel2);border-radius:4px;padding:1px 5px;font-size:12px;font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
section{display:none}section.show{display:block}
.hint{font-size:12px;color:var(--muted);margin:10px 0 16px}
</style>
</head>
<body>
<script>/*SS_DASHBOARD_DATA*/</script>
<div class="layout">
<nav>
  <h1>SpecSlice</h1>
  <button data-t="overview" class="active">概览</button>
  <button data-t="modules">业务模块</button>
  <button data-t="features">功能簇</button>
  <button data-t="checks">检查</button>
  <button data-t="dead">死代码</button>
  <button data-t="questions">待澄清</button>
  <button data-t="purity">纯度</button>
  <div class="meta" id="meta"></div>
</nav>
<main>
  <section id="t-overview" class="show"></section>
  <section id="t-modules"></section>
  <section id="t-features"></section>
  <section id="t-checks"></section>
  <section id="t-dead"></section>
  <section id="t-questions"></section>
  <section id="t-purity"></section>
</main>
</div>
<script>
(function(){
"use strict";
var D = window.__SS_DASHBOARD__ || {};
function esc(s){return String(s==null?"":s).replace(/[&<>"]/g,function(c){return{"&":"&amp;","<":"&lt;",">":"&gt;","\"":"&quot;"}[c];});}
function el(id){return document.getElementById(id);}
function card(v,k){return '<div class="card"><div class="v">'+esc(v)+'</div><div class="k">'+esc(k)+'</div></div>';}
function err(sec){return sec&&sec.error?'<div class="empty">分析失败: '+esc(sec.error)+'</div>':null;}
function empty(msg){return '<div class="empty">'+esc(msg)+'</div>';}
function table(headers,rows){
  if(!rows.length)return empty("（无数据）");
  var h='<table><thead><tr>'+headers.map(function(x){return '<th>'+esc(x)+'</th>';}).join("")+'</tr></thead><tbody>';
  h+=rows.map(function(r){return '<tr>'+r.map(function(c){return '<td>'+c+'</td>';}).join("")+'</tr>';}).join("");
  return h+'</tbody></table>';
}
function searchable(containerId,inputPh,renderRows){
  var box='<input class="search" placeholder="'+esc(inputPh)+'" id="'+containerId+'-q"><div id="'+containerId+'-body"></div>';
  return {html:box,bind:function(){
    var q=el(containerId+'-q'),body=el(containerId+'-body');
    var update=function(){body.innerHTML=renderRows((q.value||"").toLowerCase());};
    q.addEventListener('input',update);update();
  }};
}

// ---- meta ----
var meta=D.meta||{};
el('meta').innerHTML='repo: '+esc(meta.repo||'?')+'<br>'+esc(meta.tool||'')+'<br>'+esc(meta.generated_at||'');

// ---- overview ----
(function(){
  var s=el('t-overview'); var o=D.overview||{};
  var e=err(o); if(e){s.innerHTML='<h2>概览</h2>'+e;return;}
  var checks=D.checks&&D.checks.findings?D.checks.findings.length:0;
  var dead=D.dead_code&&D.dead_code.stats?D.dead_code.stats.candidates||((D.dead_code.candidates||[]).length):((D.dead_code||{}).candidates||[]).length;
  var qs=D.questions&&D.questions.questions?D.questions.questions.length:0;
  var html='<h2>概览</h2><div class="cards">'
    +card(o.nodes||0,'图节点')+card(o.edges||0,'图边')
    +card((D.modules&&D.modules.modules||[]).length,'业务模块')
    +card((D.features&&D.features.clusters||[]).length,'功能簇')
    +card(checks,'检查发现')+card(dead,'死代码候选')+card(qs,'待澄清问题')
    +'</div>';
  var nk=o.node_kinds||{},ek=o.edge_kinds||{};
  var nkRows=Object.keys(nk).sort(function(a,b){return nk[b]-nk[a];}).slice(0,18).map(function(k){return [esc(k),'<span class="num">'+nk[k]+'</span>'];});
  var ekRows=Object.keys(ek).sort(function(a,b){return ek[b]-ek[a];}).map(function(k){return [esc(k),'<span class="num">'+ek[k]+'</span>'];});
  html+='<div style="display:grid;grid-template-columns:1fr 1fr;gap:14px"><div><h2>节点类型</h2>'+table(['kind','count'],nkRows)+'</div><div><h2>边类型</h2>'+table(['kind','count'],ekRows)+'</div></div>';
  html+='<p class="hint">交互式代码图请用 <code>specslice graph --format web</code> 生成。</p>';
  s.innerHTML=html;
})();

// ---- modules ----
(function(){
  var s=el('t-modules'); var m=D.modules||{};
  var e=err(m); if(e){s.innerHTML='<h2>业务模块</h2>'+e;return;}
  var mods=m.modules||[];
  var sec=searchable('mods','按模块名 / 路径过滤…',function(q){
    var rows=mods.filter(function(x){
      return !q||String(x.name||'').toLowerCase().indexOf(q)>=0||String(x.path_prefix||'').toLowerCase().indexOf(q)>=0;
    }).map(function(x){
      var coh=Math.round((x.cohesion||0)*100);
      return [esc(x.name)+'<br><span class="muted">'+esc(x.id)+'</span>',
        esc(x.path_prefix||''),
        '<span class="num">'+(x.file_count||0)+'</span>',
        '<span class="num">'+(x.symbol_count||0)+'</span>',
        '<span class="num">'+(x.test_count||0)+'</span>',
        '<div class="bar" title="'+coh+'%"><i style="width:'+coh+'%"></i></div>',
        (x.depends_on||[]).slice(0,6).map(function(d){return '<span class="tag">'+esc(d)+'</span>';}).join("")];
    });
    return table(['模块','路径','文件','符号','测试','内聚','依赖'],rows);
  });
  s.innerHTML='<h2>业务模块 ('+mods.length+')</h2>'+sec.html; sec.bind();
})();

// ---- features ----
(function(){
  var s=el('t-features'); var f=D.features||{};
  var e=err(f); if(e){s.innerHTML='<h2>功能簇</h2>'+e;return;}
  var cs=f.clusters||[];
  var sec=searchable('feats','按簇名 / 路径过滤…',function(q){
    var rows=cs.filter(function(c){
      return !q||String(c.name||'').toLowerCase().indexOf(q)>=0||String(c.seed_path||'').toLowerCase().indexOf(q)>=0;
    }).map(function(c){
      return [esc(c.name),esc(c.seed_path||''),
        '<span class="num">'+(c.node_count||0)+'</span>',
        '<span class="num">'+(c.seed_score||0)+'</span>',
        (c.roles||[]).map(function(r){return '<span class="tag ok">'+esc(r)+'</span>';}).join("")||'<span class="muted">—</span>'];
    });
    return table(['功能簇','种子文件','节点','种子分','框架角色'],rows);
  });
  s.innerHTML='<h2>功能簇 ('+cs.length+')</h2>'+sec.html; sec.bind();
})();

// ---- checks ----
(function(){
  var s=el('t-checks'); var c=D.checks||{};
  var e=err(c); if(e){s.innerHTML='<h2>检查</h2>'+e;return;}
  var fs=c.findings||[];
  var rows=fs.map(function(x){
    var sev=String(x.severity||'').toLowerCase();
    var cls=sev==='error'?'err':(sev==='warning'?'warn':'ok');
    return ['<span class="tag '+cls+'">'+esc(sev||'info')+'</span>',esc(x.code||''),esc(x.message||''),esc(x.path||'')];
  });
  s.innerHTML='<h2>检查 ('+fs.length+')</h2>'+(rows.length?table(['级别','代码','信息','路径'],rows):empty('0 findings — 文档与代码当前一致'));
})();

// ---- dead code ----
(function(){
  var s=el('t-dead'); var d=D.dead_code||{};
  var e=err(d); if(e){s.innerHTML='<h2>死代码</h2>'+e;return;}
  var st=d.stats||{};
  var cards='<div class="cards">'+card(st.total_code_symbols||0,'总符号')+card(st.entrypoints||0,'入口点')+card(st.reachable||0,'可达')+card((d.candidates||[]).length,'候选')+'</div>';
  var cs=d.candidates||[];
  var sec=searchable('dead','按符号名 / 路径过滤…',function(q){
    var rows=cs.filter(function(x){
      return !q||String(x.label||'').toLowerCase().indexOf(q)>=0||String(x.path||'').toLowerCase().indexOf(q)>=0;
    }).slice(0,500).map(function(x){
      var conf=String(x.confidence||'').toLowerCase();
      var cls=conf==='high'?'err':(conf==='medium'?'warn':'ok');
      return ['<span class="tag '+cls+'">'+esc(conf)+'</span>',esc(x.label||''),esc(x.kind||''),esc(x.path||''),(x.reasons||[]).map(esc).join('<br>')];
    });
    return table(['置信度','符号','类型','路径','原因'],rows);
  });
  s.innerHTML='<h2>死代码候选</h2>'+cards+sec.html; sec.bind();
})();

// ---- questions ----
(function(){
  var s=el('t-questions'); var qd=D.questions||{};
  var e=err(qd); if(e){s.innerHTML='<h2>待澄清</h2>'+e;return;}
  var qs=qd.questions||[];
  var rows=qs.map(function(x){
    var sev=String(x.severity||'').toLowerCase();
    return ['<span class="tag '+(sev==='warn'?'warn':'ok')+'">'+esc(x.category||'')+'</span>',esc(x.prompt||''),esc(x.artifact_id||''),esc(x.path||'')];
  });
  s.innerHTML='<h2>待澄清问题 ('+qs.length+')</h2>'+(rows.length?table(['类别','问题','工件','路径'],rows):empty('没有待澄清的问题'));
})();

// ---- purity ----
(function(){
  var s=el('t-purity'); var p=D.purity||{};
  var e=err(p); if(e){s.innerHTML='<h2>纯度</h2>'+e;return;}
  var st=p.stats||{};
  var cards='<div class="cards">'+card(st.analyzed||0,'已分析')+card(st.pure||0,'纯函数')+card(st.impure||0,'有副作用')+card(st.unknown||0,'未知')+'</div>';
  var rows=(p.impure_sample||[]).map(function(x){
    return [esc(x.name||''),esc(x.path||''),(x.signals||[]).map(function(g){return '<span class="tag warn">'+esc(g)+'</span>';}).join("")];
  });
  s.innerHTML='<h2>纯度</h2>'+cards+'<h2>副作用样本（前 100）</h2>'+table(['符号','路径','副作用信号'],rows);
})();

// ---- tabs ----
var btns=document.querySelectorAll('nav button[data-t]');
btns.forEach(function(b){
  b.addEventListener('click',function(){
    btns.forEach(function(x){x.classList.remove('active');});
    document.querySelectorAll('main section').forEach(function(x){x.classList.remove('show');});
    b.classList.add('active');
    el('t-'+b.getAttribute('data-t')).classList.add('show');
  });
});
})();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_neutralises_script_close_in_data() {
        let html = render_dashboard_html(r#"{"x":"</script><script>alert(1)"}"#);
        assert!(!html.contains("</script><script>alert"));
        assert!(html.contains("window.__SS_DASHBOARD__"));
    }

    #[test]
    fn template_keeps_data_slot_and_offline_invariants() {
        assert!(DASHBOARD_TEMPLATE.contains("/*SS_DASHBOARD_DATA*/"));
        assert!(!DASHBOARD_TEMPLATE.contains("src=\"http"));
        assert!(!DASHBOARD_TEMPLATE.contains("href=\"http"));
    }

    /// issues2.md #40: dashboards get shared (CI artifacts, issues). The
    /// payload must identify the repo by name only — never by the host's
    /// absolute path, which leaks usernames and directory layout.
    #[test]
    fn collect_embeds_repo_name_not_host_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("acme-app");
        std::fs::create_dir_all(repo.join(".specslice")).unwrap();
        let data = collect(&repo);
        let payload = serde_json::to_string(&data).unwrap();
        assert_eq!(data["meta"]["repo"], "acme-app");
        let host_prefix = dir.path().to_string_lossy().to_string();
        assert!(
            !payload.contains(&host_prefix),
            "absolute host path must not appear in the dashboard payload"
        );
    }
}
