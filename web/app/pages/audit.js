import {$,$$,hhmm,esc,ICON,DAYS,MONTHS} from '../core/util.js';
import {api} from '../core/api.js';
import {CAT,AUDIT,AUDITLOAD,loadAudit} from '../core/store.js';
import {SHEET,openSheet} from '../ui/sheets.js';

const auditSheets={
  history(){
    const groups={};for(const r of AUDIT){const k=r.t.toDateString()===new Date().toDateString()?'Today':DAYS[r.t.getDay()]+' '+r.t.getDate()+' '+MONTHS[r.t.getMonth()];(groups[k]=groups[k]||[]).push(r)}
    const consequence=r=>{const d=CAT[r.rawAction];return d?[d.description].concat(d.consequences||[]).join(' '):''};
    const html=`${AUDIT.length?`<div class="hist">${Object.entries(groups).map(([k,rows])=>`<h3>${k}</h3>${rows.map(r=>{const c=consequence(r);return `<button class="hrow" data-h="${r.id}" aria-expanded="${r.open}"><time class="num">${hhmm(r.t)}</time><div class="t"><b>${esc(r.action)}</b>${/simulated/.test(r.result)?'<span class="tag">dry-run</span>':''}<small>${esc([r.who,r.target,r.channel,r.guard&&r.guard!=='none'?r.guard:'',r.result].filter(Boolean).join(' · '))}</small>${r.open?`<span class="more">${c?'About this action: '+esc(c):'No description: this was not a device action.'}${r.from&&r.from!=='-'?`<br>From ${esc(r.from)}`:''}</span>`:''}</div></button>`}).join('')}`).join('')}</div>`:`<p class="quiet">${AUDITLOAD.at?'Nothing asked for yet. Everything you ask for appears here with what the device answered.':'Loading…'}</p>`}
      <div class="rows"><div class="rowi">${ICON('check')}<div class="t"><b>Check this log</b><small id="au-verify">Confirms nothing here was changed or removed</small></div><button class="pill" id="au-go">Check</button></div></div>`;
    const redraw=()=>{if(SHEET.type==='history')openSheet('history',null,{replace:true,focus:'keep'})};
    return {title:'History',html,mount(root){if(Date.now()-AUDITLOAD.at>4000)loadAudit().then(redraw).catch(()=>{});
      $$('[data-h]',root).forEach(b=>b.onclick=()=>{const r=AUDIT.find(x=>String(x.id)===b.dataset.h);r.open=!r.open;redraw()});
      $('#au-go',root).onclick=async()=>{const o=$('#au-verify',root);o.textContent='Checking…';try{const r=await api('GET','/audit/verify');o.textContent=r.ok?'Intact: every row and the chain check out.':'Not intact. Something was changed or removed.'}catch(e){o.textContent=e.message}}}};
  },
};

export {auditSheets};
