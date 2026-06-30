/* XERJ.AI interactive UX · filters · tabs · sliders · theme.
   Loaded at end of every marketing page. Vanilla — no framework. */
(function () {
  'use strict';
  const $  = (sel, root) => (root || document).querySelector(sel);
  const $$ = (sel, root) => [...(root || document).querySelectorAll(sel)];

  /* ---------- theme toggle ---------- */
  const root = document.documentElement;
  $$('[data-theme]').forEach(btn => {
    btn.addEventListener('click', () => {
      const t = btn.getAttribute('data-theme');
      if (!t || !btn.tagName || btn.tagName.toLowerCase() !== 'button') return;
      root.setAttribute('data-theme', t);
      $$('[data-theme]').forEach(b => b.classList.toggle('active', b.getAttribute('data-theme') === t));
    });
  });

  /* ---------- chip filters ---------- */
  /* Markup contract:
       <div class="chips" data-filter="{group}">
         <button class="chip active" data-value="all">ALL</button>
         <button class="chip" data-value="finserv">FINSERV</button>
         ...
       </div>
       <div data-filterable="{group}" data-tags="finserv healthcare">...</div>
     Clicking a chip sets that value active; items NOT matching the selected value
     get .dim (opacity 0.25, pointer-events none). "all" clears filters. */
  $$('.chips[data-filter]').forEach(group => {
    const name = group.getAttribute('data-filter');
    group.addEventListener('click', e => {
      const chip = e.target.closest('.chip');
      if (!chip) return;
      $$('.chip', group).forEach(c => c.classList.remove('active'));
      chip.classList.add('active');
      const value = chip.getAttribute('data-value');
      $$(`[data-filterable="${name}"]`).forEach(item => {
        const tags = (item.getAttribute('data-tags') || '').split(/\s+/);
        const match = value === 'all' || tags.includes(value);
        item.classList.toggle('dim', !match);
      });
    });
  });

  /* ---------- tabs / panes ---------- */
  /* Markup contract:
       <div class="tabs" data-tabs="{group}">
         <button class="tab active" data-pane="single">SINGLE</button>
         <button class="tab" data-pane="ha">HA</button>
       </div>
       <div class="pane active" data-pane="single" data-group="{group}">...</div>
       <div class="pane"        data-pane="ha"     data-group="{group}">...</div> */
  $$('.tabs[data-tabs]').forEach(group => {
    const name = group.getAttribute('data-tabs');
    group.addEventListener('click', e => {
      const tab = e.target.closest('.tab');
      if (!tab) return;
      $$('.tab', group).forEach(t => t.classList.remove('active'));
      tab.classList.add('active');
      const paneId = tab.getAttribute('data-pane');
      $$(`.pane[data-group="${name}"]`).forEach(p => {
        p.classList.toggle('active', p.getAttribute('data-pane') === paneId);
      });
    });
  });

  /* ---------- sliders (cost-savings, catalog-scale) ---------- */
  /* Markup contract:
       <input type="range" data-slider="corpus" min="1" max="10" value="5"
              data-format="{value}M events">
       <span data-slider-out="corpus"></span>
       <div data-slider-update="corpus" data-at-1="..." data-at-5="..." data-at-10="...">
       The output element reflects the slider value with formatting.
       Any element with data-slider-update matching the slider name will
       swap its innerHTML for its data-at-{value} attribute. */
  $$('input[type="range"][data-slider]').forEach(slider => {
    const name = slider.getAttribute('data-slider');
    const fmt  = slider.getAttribute('data-format') || '{value}';
    const out  = $$(`[data-slider-out="${name}"]`);
    const update = () => {
      const v = slider.value;
      out.forEach(o => o.textContent = fmt.replace('{value}', v));
      $$(`[data-slider-update="${name}"]`).forEach(target => {
        const at = target.getAttribute('data-at-' + v);
        if (at != null) target.innerHTML = at;
      });
    };
    slider.addEventListener('input', update);
    update();
  });
})();
