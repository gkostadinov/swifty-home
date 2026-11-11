import {$,esc} from '../core/util.js';

function toast(text,kind,actions){
  const t=document.createElement('div');t.className='g toast '+(kind||'');t.innerHTML=`<span>${esc(text)}</span>`;
  (actions||[]).forEach(x=>{const b=document.createElement('button');b.className='btn';b.textContent=x.l;b.onclick=()=>{x.fn();t.remove()};t.appendChild(b)});
  $('#toasts').appendChild(t);setTimeout(()=>t.remove(),kind==='fail'?9000:actions?7000:4000);
}

export {toast};
