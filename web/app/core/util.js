/* ============================================================ utilities */
const $=(s,r=document)=>r.querySelector(s);
const $$=(s,r=document)=>Array.from(r.querySelectorAll(s));
const clamp=(v,a,b)=>Math.max(a,Math.min(b,v));
const rnd=(a,b)=>a+Math.random()*(b-a);
const pad=n=>String(n).padStart(2,'0');
const hhmm=d=>pad(d.getHours())+':'+pad(d.getMinutes());
const esc=s=>String(s).replace(/[&<>"]/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[c]));
const store={get(k,d){try{const v=localStorage.getItem('gh.'+k);return v==null?d:JSON.parse(v)}catch(e){return d}},set(k,v){try{localStorage.setItem('gh.'+k,JSON.stringify(v))}catch(e){}}};
const reduced=matchMedia('(prefers-reduced-motion: reduce)').matches;
const isPhone=()=>matchMedia('(max-width: 699px)').matches;
const ICON=n=>`<svg class="ic" aria-hidden="true"><use href="#i-${n}"/></svg>`;
const PEND='<span class="pend" aria-hidden="true"></span>';
let uid=0;const nid=()=>'a'+(++uid);
const DAYS=['Sunday','Monday','Tuesday','Wednesday','Thursday','Friday','Saturday'];
const MONTHS=['January','February','March','April','May','June','July','August','September','October','November','December'];

/* a physically believable spring, written as a linear() easing where the browser understands it */
(function spring(){
  if(reduced)return;
  try{
    if(!CSS.supports('transition-timing-function','linear(0, 1)'))return;
    const k=300,c=26,dt=1/60,pts=[];let d=-1,v=0;
    for(let i=0;i<=48;i++){pts.push((1+d).toFixed(4));const a=-k*d-c*v;v+=a*dt;d+=v*dt}
    pts.push('1');
    document.documentElement.style.setProperty('--spring',`linear(${pts.join(',')})`);
  }catch(e){}
})();

export {$,$$,clamp,rnd,pad,hhmm,esc,store,reduced,isPhone,ICON,PEND,nid,DAYS,MONTHS};
