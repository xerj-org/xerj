"""The "Agent score" on hub.xerj.org: structured reviews agents send for a
corpus, served by /api/corpus-feedback (functions/api/corpus-feedback.js).

Kept apart from the human hub ★ on purpose: different section, different
words ("agent score", "agent reviews"), no star glyph. The panel ships with an
honest default ("no agent reviews yet" only once the API says so; "not being
collected yet" when the endpoint is disabled) and never invents a number.
"""

STYLE = """
/* agent score (functions/api/corpus-feedback.js): separate from hub stars */
.agentscore .ashead{display:flex;gap:var(--sp-6);align-items:baseline;flex-wrap:wrap}
.agentscore .asnum{font-family:var(--font-data);font-size:var(--fs-48,48px);font-weight:700;line-height:1}
.agentscore .asnum small{font-size:var(--fs-13);color:var(--z-mute);font-weight:400}
.agentscore .asconf{font-family:var(--font-data);font-size:var(--fs-11);text-transform:uppercase;letter-spacing:var(--track-ui);
border:1px solid var(--z-line);padding:.15em .6em;color:var(--z-mute)}
.agentscore .asgrid{display:grid;grid-template-columns:repeat(auto-fill,minmax(180px,1fr));gap:var(--sp-4);margin:var(--sp-4) 0}
.agentscore .asgrid b{display:block;font-family:var(--font-data);font-size:var(--fs-20,20px)}
.agentscore .asgrid span{color:var(--z-mute);font-size:var(--fs-11);text-transform:uppercase;letter-spacing:var(--track-label)}
.agentscore [hidden]{display:none}
.badge.agent{color:var(--z-cmp);border-color:var(--z-cmp)}
.sortbar{display:flex;gap:var(--sp-2);align-items:center;margin-top:var(--sp-4)}
"""


def section(slug, esc):
    s = esc(slug.lower())
    return f'''<section class="detail agentscore" id="agent-score" data-corpus="{s}"><h2>Agent score</h2>
<p class="note">From structured reviews that AI agents send after they use this corpus: through
<code>xerj hub pull</code>, MCP queries, or the <code>xerj_corpus_feedback</code> tool. Each review
rates usefulness 1&ndash;5, whether the answer was found, and freshness. This is
<b>not</b> the human hub star count shown above. The two are never mixed.</p>
<div class="asbody"><p class="note" data-state="loading">Loading agent reviews&hellip;</p></div>
<p class="note">Score = Bayesian mean of usefulness (prior 3.0 with weight 5), so a few reviews
cannot swing it. One voice per installation per corpus per week; abnormal-volume
reviewers are dropped. Coverage gaps appear only once 3 or more agents report the same one.
<a href="https://github.com/xerj-org/xerj/blob/corpus-hub/functions/api/corpus-feedback.js">How it works</a>.</p>
</section>'''


# Fills #agent-score from /api/corpus-feedback?corpus=. All API text goes in
# via textContent, never as HTML.
PANEL_JS = """<script>
(function(){
  var sec=document.getElementById('agent-score');if(!sec)return;
  var body=sec.querySelector('.asbody');
  function el(t,c,txt){var e=document.createElement(t);if(c)e.className=c;if(txt!=null)e.textContent=txt;return e}
  function note(txt){body.textContent='';body.appendChild(el('p','note',txt))}
  function stat(grid,v,label){var d=el('div');d.appendChild(el('b',null,v));d.appendChild(el('span',null,label));grid.appendChild(d)}
  fetch('/api/corpus-feedback?corpus='+encodeURIComponent(sec.dataset.corpus)).then(function(r){return r.json()}).then(function(a){
    if(!a||!a.ok){note('Agent reviews are unavailable right now.');return}
    if(!a.enabled){note('Agent reviews are not being collected yet.');return}
    if(!a.reviews){note('No agent reviews yet. Scores appear here once agents that use this corpus send a review.');return}
    body.textContent='';
    var head=el('div','ashead'),num=el('div','asnum',a.score.toFixed(2));
    num.appendChild(el('small',null,' / 5 agent score'));head.appendChild(num);
    head.appendChild(el('span','asconf',a.confidence+' confidence'));
    var auto=a.agent_reviews==null?null:a.reviews-a.agent_reviews;
    head.appendChild(el('span','note',a.reviews+' review'+(a.reviews===1?'':'s')+(auto?' ('+a.agent_reviews+' written by agents, '+auto+' automatic from client signals)':'')+' · raw mean '+a.raw_mean.toFixed(2)));
    body.appendChild(head);
    var g=el('div','asgrid');
    stat(g,a.found.rate==null?'n/a':Math.round(a.found.rate*100)+'%','found rate ('+a.found.yes+' yes · '+a.found.partial+' partial · '+a.found.no+' no)');
    stat(g,a.freshness_median==null?'n/a':a.freshness_median+' / 5','median freshness');
    var p=a.problems||{},pk=Object.keys(p).filter(function(k){return p[k]>0});
    stat(g,pk.length?pk.map(function(k){return k+' '+p[k]}).join(' · '):'none','problems reported');
    body.appendChild(g);
    if(a.families&&a.families.length){
      var t=el('table'),hr=el('tr');['agent family','reviews','score'].forEach(function(h){hr.appendChild(el('th',null,h))});t.appendChild(hr);
      a.families.forEach(function(f){var tr=el('tr');tr.appendChild(el('td','mono',f.family));tr.appendChild(el('td','mono',String(f.reviews)));
        tr.appendChild(el('td','mono',f.score==null?'(fewer than '+a.k_min+' each)':f.score.toFixed(2)));t.appendChild(tr)});
      body.appendChild(t);
    }
    if(a.coverage_gaps&&a.coverage_gaps.length){
      body.appendChild(el('p',null,'Coverage gaps agents reported (3+ agents each):'));
      var ul=el('ul');a.coverage_gaps.forEach(function(x){ul.appendChild(el('li',null,x.gap+' ('+x.reviews+')'))});body.appendChild(ul);
    }
  }).catch(function(){note('Agent reviews are unavailable right now.')});
})();
</script>"""


SORT_BAR = ('<div class="sortbar" id="agentsort" hidden><span class="kicker">SORT</span>'
            '<button class="chip on" data-sort="default">default</button>'
            '<button class="chip" data-sort="agent">agent score</button></div>')

# Category pages: an "agent N.NN (n)" badge on each reviewed card, and a sort
# by agent score (unreviewed corpora last, original order kept among ties).
# Shown only when /api/corpus-feedback is enabled and has at least one score.
CARDS_JS = """<script>
(function(){
  var bar=document.getElementById('agentsort'),grid=document.querySelector('.grid');if(!bar||!grid)return;
  fetch('/api/corpus-feedback').then(function(r){return r.json()}).then(function(r){
    if(!r||!r.ok||!r.enabled||!r.scores||!Object.keys(r.scores).length)return;
    var cards=[].slice.call(grid.querySelectorAll('.card[data-slug]'));
    cards.forEach(function(c,i){c.dataset.i=i;var s=r.scores[c.dataset.slug];if(!s)return;
      var b=document.createElement('span');b.className='badge agent';b.title='agent score (Bayesian, '+s.confidence+' confidence), not hub stars';
      b.textContent='agent '+s.score.toFixed(2)+' ('+s.reviews+')';var bs=c.querySelector('.cbadges');if(bs)bs.appendChild(b)});
    function key(c){var s=r.scores[c.dataset.slug];return s?s.score:-1}
    bar.hidden=false;
    bar.querySelectorAll('[data-sort]').forEach(function(btn){btn.addEventListener('click',function(){
      bar.querySelectorAll('[data-sort]').forEach(function(b){b.classList.toggle('on',b===btn)});
      var by=btn.dataset.sort;cards.slice().sort(function(a,b){return by==='agent'?(key(b)-key(a))||(a.dataset.i-b.dataset.i):a.dataset.i-b.dataset.i})
        .forEach(function(c){grid.appendChild(c)});
    })});
  }).catch(function(){});
})();
</script>"""
