import {$,esc,ICON,PEND} from '../core/util.js';
import {api,enc} from '../core/api.js';
import {toast} from '../ui/toast.js';
import {fmtDay} from '../ui/sheets.js';

const memorySheets={
  /* what Swifty remembers about you and about the home, what it has learned, and the week's review */
  memory(){
    const html=`<div id="mem-body" style="display:flex;flex-direction:column;gap:14px"><p class="quiet">${PEND}</p></div>`;
    return {title:'Swifty’s memory',html,mount(root){
      const body=$('#mem-body',root);
      const line=(x,can)=>`<div class="rowi">${ICON(x.house?'rooms':'person')}<div class="t"><b>${esc(x.text)}</b><small>${esc(fmtDay(x.created_at))}</small></div>${can?`<button class="pill" data-memforget="${esc(x.id)}">Remove</button>`:''}</div>`;
      const paint=m=>{const learned=m.learned||[],r=m.review;
        body.innerHTML=`<h3 class="roomhead" style="font-size:17px">About you</h3>
          <div class="rows">${m.mine.map(x=>line(x,true)).join('')||'<p class="quiet">Nothing yet. Tell Swifty “remember that …”, or add it here.</p>'}</div>
          <input class="field" id="mem-new" placeholder="Something Swifty should know about you" aria-label="Something Swifty should remember about you" maxlength="200" autocomplete="off">
          <div class="acts"><button class="btn" id="mem-add">${ICON('plus')}Remember</button></div>
          <h3 class="roomhead" style="font-size:17px">About the home</h3>
          <div class="rows">${m.house.map(x=>line(x,m.house_editable)).join('')||'<p class="quiet">Nothing yet.</p>'}</div>
          ${m.house_editable?`<input class="field" id="mem-house" placeholder="Something everyone’s Swifty should know" aria-label="Something Swifty should remember about the home" maxlength="200" autocomplete="off"><div class="acts"><button class="btn" id="mem-add-house">${ICON('plus')}Remember for the home</button></div>`:''}
          ${r?`<h3 class="roomhead" style="font-size:17px">Learned phrases</h3>
          <div class="rows">${learned.map(x=>`<div class="rowi">${ICON('swifty')}<div class="t"><b>“${esc(x.phrase)}”</b><small>${esc(x.label)}${x.uses?' · used '+x.uses+' time'+(x.uses===1?'':'s'):''}</small></div><button class="pill" data-unlearn="${esc(x.id)}">Remove</button></div>`).join('')||'<p class="quiet">None yet. A command Swifty works out once runs at once the next time.</p>'}</div>
          <h3 class="roomhead" style="font-size:17px">This week</h3>
          <div class="rows"><div class="rowi">${ICON('clock')}<div class="t"><b>${r.learned} learned · ${r.worked_out} worked out · ${r.corrected} corrected</b><small>Commands the quick parsers missed, and what became of them</small></div></div>${r.not_understood.map(([p,n])=>`<div class="rowi">${ICON('x')}<div class="t"><b>“${esc(p)}”</b><small>not understood${n>1?' · '+n+' times':''}</small></div></div>`).join('')}</div>`:''}
          <p class="p2">Swifty reads these on every turn. They are notes, never permissions, and nothing that looks like a password or a key is kept.${r?' A summary of each week goes into History.':''}</p>`};
      const load=()=>api('GET','/memory').then(paint).catch(e=>{if(e.status!==401&&e.status!==423)body.innerHTML=`<p class="quiet">${esc(e.message)}</p>`});
      const add=async b=>{const house=b.id==='mem-add-house';const inp=$(house?'#mem-house':'#mem-new',body);const text=inp.value.trim();if(!text){inp.focus();return}
        b.disabled=true;try{await api('POST','/memory',{text,house});inp.value='';await load()}catch(e){if(e.status!==401&&e.status!==423)toast(e.message,'fail')}finally{b.disabled=false}};
      /* one delegated handler on the body, so a repaint can never strip it */
      body.addEventListener('click',async e=>{const b=e.target.closest('#mem-add,#mem-add-house');if(b){add(b);return}
        const f=e.target.closest('[data-memforget],[data-unlearn]');if(!f)return;f.disabled=true;
        try{await api('DELETE',f.dataset.memforget?'/memory/'+enc(f.dataset.memforget):'/learned/'+enc(f.dataset.unlearn));await load()}catch(e){f.disabled=false;if(e.status!==401&&e.status!==423)toast(e.message,'fail')}});
      body.addEventListener('keydown',e=>{if(e.key!=='Enter')return;const t=e.target.closest('#mem-new,#mem-house');if(!t)return;e.preventDefault();const b=$(t.id==='mem-house'?'#mem-add-house':'#mem-add',body);if(b)add(b)});
      load()}};
  },
};

export {memorySheets};
