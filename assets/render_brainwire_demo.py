"""Generate the synthetic Cypher walkthrough (Pillow, ffmpeg; no database writes).

Run: uv run --no-project --with pillow python assets/render_brainwire_demo.py
The animation is an explanatory diagram, not a recording of the terminal UI.
"""
from pathlib import Path
import math
import subprocess
import tempfile
from PIL import Image, ImageDraw, ImageFont, ImageFilter

OUT = Path(__file__).resolve().parent
W, H, FPS, SECONDS = 1280, 680, 12, 12
BG, PANEL, GREEN, CYAN = '#080e0c', '#0b1510', '#65ff54', '#58dbcc'
TEXT, MUTED, BORDER = '#e8f0eb', '#8ca89a', '#203b2c'
MONO = '/System/Library/Fonts/SFNSMono.ttf'
SANS = '/System/Library/Fonts/SFNS.ttf'

def font(size, mono=False):
    return ImageFont.truetype(MONO if mono else SANS, size)

def center(d, xy, text, size, color, mono=False):
    box = d.textbbox((0, 0), text, font=font(size, mono))
    d.text((xy[0]-(box[2]-box[0])/2, xy[1]), text, font=font(size, mono), fill=color)

def clamp(x):
    return max(0, min(1, x))

NODES = {
    'Ada': (690, 251, 'Customer'),
    'Maya': (720, 463, 'Customer'),
    'Noah': (1085, 453, 'Customer'),
    'beta': (960, 275, 'Segment'),
    'early access': (1120, 145, 'Segment'),
}
EDGES = [('Ada', 'beta'), ('Maya', 'beta'), ('Noah', 'early access')]
QUERY = ["MATCH (c:Customer)", "      -[:IN_SEGMENT]->(s:Segment)", "WHERE s.name = 'beta'", 'RETURN c.name, s.name']

def frame(t):
    im = Image.new('RGB', (W,H), BG)
    d = ImageDraw.Draw(im)
    d.rounded_rectangle((12,12,W-12,H-12), 22, fill=PANEL, outline=BORDER, width=2)
    for x in range(32,W-20,24):
        for y in range(96,H-70,24):
            d.point((x,y), fill='#15271c')
    d.line((32,82,W-32,82), fill=BORDER)
    # Small graph mark matches the bright-green identity.
    for a,b in [((42,39),(58,54)),((58,54),(76,36)),((42,68),(58,54)),((58,54),(77,70))]:
        d.line((a,b), fill=GREEN, width=2)
    for x,y in [(42,39),(58,54),(76,36),(42,68),(77,70)]:
        d.ellipse((x-3,y-3,x+3,y+3),fill=GREEN)
    d.text((94,33),'BRAINWIRE',font=font(25,True),fill=TEXT)
    d.text((288,41),'CONTEXT HAS CONNECTIONS',font=font(12,True),fill=MUTED)
    d.rounded_rectangle((1033,32,1238,63),10,outline=BORDER,fill='#102417')
    center(d,(1135,40),'SYNTHETIC WALKTHROUGH',11,GREEN,True)
    d.line((500,108,500,581),fill=BORDER)
    d.text((40,110),'01  ASK WITH CYPHER',font=font(12,True),fill=GREEN)
    d.text((40,143),'Who belongs to beta?',font=font(29),fill=TEXT)
    d.text((40,189),'Follow the relationship. Return the facts.',font=font(15),fill=MUTED)
    d.rounded_rectangle((34,233,478,378),12,fill='#080f0b',outline=BORDER)
    count = int(clamp((t-.7)/3.2)*sum(map(len,QUERY)))
    for i,line in enumerate(QUERY):
        visible=line[:count]; count=max(0,count-len(line))
        d.text((50,251+i*29),visible,font=font(15,True),fill=CYAN if i<2 else TEXT)
    if t<4 and int(t*3)%2==0:
        d.rectangle((451,350,459,366),fill=GREEN)
    if t>=5.8:
        d.text((41,410),'c.name',font=font(14,True),fill=MUTED)
        d.text((255,410),'s.name',font=font(14,True),fill=MUTED)
        d.line((40,439,470,439),fill=BORDER)
        for i,name in enumerate(['Ada','Maya']):
            if t>=5.8+i*.55:
                d.text((41,457+38*i),name,font=font(19,True),fill=TEXT)
                d.text((255,457+38*i),'beta',font=font(19,True),fill=GREEN)
        if t>7:
            d.text((40,550),'2 connected records',font=font(13,True),fill=MUTED)
    d.text((540,110),'02  FOLLOW THE CONNECTIONS',font=font(12,True),fill=MUTED)
    glow=Image.new('RGBA',(W,H)); gd=ImageDraw.Draw(glow)
    for index,(a,b) in enumerate(EDGES):
        x,y,_=NODES[a]; X,Y,_=NODES[b]
        dx,dy=X-x,Y-y; length=math.hypot(dx,dy)
        start=(x+dx/length*28,y+dy/length*28)
        end=(X-dx/length*32,Y-dy/length*32)
        active=index<2 and t>4+index*.65
        color=GREEN if active else '#294335'
        d.line((start,end),fill=color,width=2 if active else 1)
        if active:
            gd.line((start,end),fill=(101,255,84,85),width=8)
            progress=((t-4-index*.65)*.6)%1
            px=start[0]+(end[0]-start[0])*progress
            py=start[1]+(end[1]-start[1])*progress
            d.ellipse((px-4,py-4,px+4,py+4),fill=GREEN)
        angle=math.atan2(dy,dx)
        arrow=[end,(end[0]-10*math.cos(angle-.4),end[1]-10*math.sin(angle-.4)),(end[0]-10*math.cos(angle+.4),end[1]-10*math.sin(angle+.4))]
        d.polygon(arrow,fill=color)
        mx,my=(x+X)/2,(y+Y)/2
        if index<2:
            d.rounded_rectangle((mx-56,my-13,mx+57,my+11),8,fill='#102017',outline=BORDER)
            center(d,(mx,my-7),'IN_SEGMENT',10,GREEN if active else MUTED,True)
    im=Image.alpha_composite(im.convert('RGBA'),glow.filter(ImageFilter.GaussianBlur(8))).convert('RGB');d=ImageDraw.Draw(im)
    for name,(x,y,kind) in NODES.items():
        selected=name in ('Ada','Maya','beta') and t>4
        c=GREEN if selected else CYAN if kind=='Segment' else '#70877a'
        r=28 if kind=='Segment' else 22
        if selected:
            d.ellipse((x-r-7,y-r-7,x+r+7,y+r+7),outline='#264f2a',width=1)
        d.ellipse((x-r,y-r,x+r,y+r),fill='#102316' if selected else '#0b1811',outline=c,width=2)
        if kind=='Customer':
            d.ellipse((x-5,y-9,x+5,y+1),outline=c,width=2)
            d.arc((x-10,y-1,x+10,y+16),185,355,fill=c,width=2)
        else:
            d.rectangle((x-8,y-8,x+8,y+8),outline=c,width=2)
        center(d,(x,y+r+13),name,21,TEXT)
        center(d,(x,y+r+41),kind,12,MUTED,True)
    d.line((32,611,W-32,611),fill=BORDER)
    d.text((40,632),'Animated illustration · synthetic data · supported Cypher pattern',font=font(12),fill=MUTED)
    d.text((1010,630),'PYTHON  →  CYPHER',font=font(13,True),fill=GREEN)
    d.rounded_rectangle((40,600,40+int(1195*(t/SECONDS)),602),1,fill=GREEN)
    return im

def main():
    with tempfile.TemporaryDirectory(prefix='brainwire-demo-frames-') as temp:
        temp=Path(temp)
        for n in range(FPS*SECONDS):
            frame(n/FPS).save(temp/f'{n:04}.png')
        frame(9).save(OUT/'brainwire-demo-poster.png')
        subprocess.run(['ffmpeg','-y','-loglevel','error','-framerate',str(FPS),'-i',str(temp/'%04d.png'),'-vf','palettegen=stats_mode=diff',str(temp/'palette.png')],check=True)
        subprocess.run(['ffmpeg','-y','-loglevel','error','-framerate',str(FPS),'-i',str(temp/'%04d.png'),'-i',str(temp/'palette.png'),'-lavfi','paletteuse=dither=bayer:bayer_scale=3','-loop','0',str(OUT/'brainwire-demo.gif')],check=True)
        subprocess.run(['ffmpeg','-y','-loglevel','error','-framerate',str(FPS),'-i',str(temp/'%04d.png'),'-c:v','libx264','-pix_fmt','yuv420p','-crf','20','-movflags','+faststart',str(OUT/'brainwire-demo.mp4')],check=True)

if __name__=='__main__':main()
