// Throwaway (#70). Paste into the built-in browser's javascript tool once, via: localStorage.gs = String.raw`<this file>`; then on each Google Maps transit page
// (hl=zh-TW, URL https://www.google.com/maps/dir/<lat,lon>/<lat,lon>/data=!4m2!4m1!3e3 = leave now) run:  eval(localStorage.gs); (await window.gscrape()).join('\n')
// Output per option: "<minutes>m W<walk minutes> ; B[bus line] from>to Nm ; M[rail/metro line] from>to Nm". Rail vs bus is a label heuristic: re-check labels that start with digits.
window.gparse=function(t){
 const k=t.split(' | ').map(x=>x.replace(/[\p{Co}\p{Cf}\u{a0}\u{3000}]/gu,' ').trim()).filter(x=>x.length>0);
 const isT=x=>/^(上午|下午)\d+:\d+$/.test(x);
 const hl=(k.find(x=>/\((?:\d+ 小時 )?\d+ 分\)/.test(x))||''); const hm=hl.match(/\((?:(\d+) 小時 )?(\d+) 分\)/); const hdr=hm?String((+(hm[1]||0))*60+(+hm[2])):'?'; let walk=0; const legs=[];
 for(let i=0;i<k.length;i++){
  if(k[i]==='步行' && /^約 \d+ 分/.test(k[i+1]||'')) walk+=+k[i+1].match(/約 (\d+) 分/)[1];
  else if(/^\d+ 分$/.test(k[i]) && /^\(\d+ 站\)$/.test(k[i+1]||'')){
    let j=i+2; while(j<k.length && !isT(k[j])) j++;
    const line=k[i-1]; const rail=/(Line|Rail|Train|Express|Limited|機場線|自強|莒光|普悠瑪|太魯閣)/.test(line);
    legs.push((rail?'M[':'B[')+line+'] '+k[i-2]+'>'+k[j+1]+' '+k[i].replace(' 分','m'));
  }
 }
 return hdr+'m W'+walk+' ; '+legs.join(' ; ');
};
window.gscrape = async function(){
 const sleep=ms=>new Promise(r=>setTimeout(r,ms));
 const waitList=async()=>{for(let q=0;q<20;q++){ if(document.querySelector('[data-trip-index]')) return true; await sleep(300);} return false;};
 await waitList(); await sleep(1500);
 const n=new Set([...document.querySelectorAll('[data-trip-index]')].map(e=>e.dataset.tripIndex)).size;
 const out=[];
 for(let i=0;i<n;i++){
  if(!document.querySelector('[data-trip-index="'+i+'"]')){await waitList(); }
  const e2=document.querySelector('[data-trip-index="'+i+'"]'); if(!e2) {out.push('(missing '+i+')'); continue;}
  e2.click(); await sleep(1200);
  let b=[...document.querySelectorAll('button')].find(b=>b.innerText.includes('詳細資料')); if(b){b.click(); await sleep(1800);}
  const m=document.querySelector('[role="main"]')||document.body;
  let t=m.innerText.replace(/\n+/g,' | ').replace(/\t+/g,'').replace(/( \| )+/g,' | ');
  out.push(window.gparse(t));
  const back=[...document.querySelectorAll('button')].find(b=>b.getAttribute('aria-label')==='返回');
  if(back){back.click(); await sleep(1200);}
  if(!document.querySelector('[data-trip-index]')){ const back2=[...document.querySelectorAll('button')].find(b=>b.getAttribute('aria-label')==='返回'); if(back2){back2.click(); await sleep(1200);} await waitList(); }
 }
 return out;
};
