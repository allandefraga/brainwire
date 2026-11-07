"""Render the final receipt with Matplotlib; never substitutes for verification."""
import json
from pathlib import Path
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt

HERE=Path(__file__).resolve().parent
report=json.loads((HERE/'mixed-100000-3600.json').read_text())
assert report['result']=='PASS' and report['load_seconds']>=3600
observations=report['observations']
plt.style.use('dark_background')
fig,axes=plt.subplots(1,2,figsize=(13,4.8),layout='constrained')
fig.patch.set_facecolor('#0b1010')
for ax in axes:
    ax.set_facecolor('#0b1010');ax.grid(alpha=.18,axis='y')
axes[0].plot([x['seconds']/60 for x in observations],[x['rss_kib']/1024 for x in observations],color='#71ed46',linewidth=1.3,label='Native process RSS')
axes[0].plot([x['seconds']/60 for x in observations],[x['poolPeakBytes']/(1<<20) for x in observations],color='#00d7df',linewidth=1.4,label='Query pool peak (includes import)')
axes[0].set(xlabel='Load time (minutes)',ylabel='MiB',title='Memory observations',xlim=(0,60),ylim=(0,None));axes[0].legend(loc='upper right',fontsize=8)
labels=['Point read','Scalar read','Write']
for i,(key,color) in enumerate([('p50_ms','#71ed46'),('p95_ms','#00d7df'),('p99_ms','#e9bf45')]):
    axes[1].bar([x+(i-1)*.24 for x in range(3)],[report[k][key] for k in ('point','scalar','writes')],width=.23,color=color,label=key.replace('_ms',''))
axes[1].set_xticks(range(3),labels);axes[1].set(ylabel='Milliseconds',title='End-to-end Bolt latency');axes[1].legend(fontsize=8)
fig.suptitle('Brainwire · 100k synthetic nodes · 1 hour · 4 readers + 1 writer',fontsize=14)
fig.supxlabel('Single local process, shared host. Graph-wide service reads; not governed-ACL or distributed performance.',fontsize=8)
fig.savefig(HERE/'endurance.png',dpi=150)
fig.savefig(HERE/'endurance.svg')

svg=HERE/'endurance.svg'
svg.write_text('\n'.join(line.rstrip() for line in svg.read_text().splitlines())+'\n')
