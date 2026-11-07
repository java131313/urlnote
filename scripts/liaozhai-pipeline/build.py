# -*- coding: utf-8 -*-
"""聊斋笔记批量构建器:md + 插图 SVG + 上传文件库(2~20 篇)。"""
import json, subprocess, os
from urllib.parse import quote

INK = "#2b3a67"; PAPER = "#f8f3e6"; ACC = "#8a2f2f"; GOLD = "#f0a830"; BLUE = "#3498db"

def frame(main_svg, caption):
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 320 220" width="640">\n'
            f'<rect width="320" height="220" fill="{PAPER}"/>\n'
            f'<rect x="6" y="6" width="308" height="208" fill="none" stroke="{INK}" stroke-width="2"/>\n'
            f'<rect x="10" y="10" width="300" height="200" fill="none" stroke="{INK}" stroke-width="0.7" opacity="0.6"/>\n'
            '<circle cx="268" cy="46" r="22" fill="#f5e6b8"/>\n'
            '<circle cx="262" cy="41" r="3.5" fill="#e8d49a" opacity="0.7"/>\n'
            '<g fill="#f5e6b8" opacity="0.7">'
            '<circle cx="40" cy="26" r="1.3"/><circle cx="90" cy="16" r="1"/>'
            '<circle cx="140" cy="26" r="1.2"/><circle cx="200" cy="14" r="1"/></g>\n'
            + main_svg + '\n'
            f'<text x="160" y="204" font-size="9" fill="#6b7699" text-anchor="middle" font-family="serif">{caption}</text>\n'
            '</svg>')

def s_path(d, w=2.2, color=INK, fill="none"):
    return f'<path d="{d}" stroke="{color}" stroke-width="{w}" fill="{fill}" stroke-linecap="round"/>'

MOTIFS = {
 "ear": s_path("M120 70 q-24 40 0 80 q10 20 26 8 q14 -12 2 -26 q-10 -12 -2 -24 q12 -16 0 -34 q-12 -16 -26 -4z") + s_path("M132 96 q-8 22 4 40", 1.6, ACC),
 "tree": s_path("M160 180 L160 90 M160 130 L120 100 M160 130 L200 96 M160 110 L138 88 M160 110 L186 84", 3) + s_path("M120 100 q-8 -10 -2 -18 M200 96 q10 -8 4 -16", 2) + f'<g stroke="{ACC}" stroke-width="1.6" fill="none"><path d="M118 102 l6 8 M124 104 l4 8 M202 98 l5 8"/></g>',
 "fountain": s_path("M90 190 L230 190 M110 190 L110 150 L210 150 L210 190", 2.2) + f'<g stroke="{BLUE}" stroke-width="2" fill="none" opacity="0.85"><path d="M160 150 q-30 -46 -64 -52"/><path d="M160 150 q30 -46 64 -52"/><path d="M160 150 q0 -60 0 -72"/></g>' + f'<circle cx="160" cy="72" r="9" fill="{INK}" opacity="0.85"/>',
 "eye": f'<ellipse cx="160" cy="110" rx="72" ry="34" fill="none" stroke="{INK}" stroke-width="2.6"/>' + f'<circle cx="160" cy="110" r="17" fill="none" stroke="{INK}" stroke-width="2.2"/>' + f'<circle cx="160" cy="110" r="7" fill="{INK}"/>' + '<circle cx="166" cy="104" r="2.6" fill="#fff"/>' + s_path("M88 96 q70 -30 144 0", 1.4, ACC),
 "mural": f'<rect x="86" y="56" width="148" height="108" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<rect x="94" y="64" width="132" height="92" fill="none" stroke="{INK}" stroke-width="0.8" opacity="0.5"/>' + s_path("M160 148 q-12 -22 0 -44 q12 22 0 44z", 1.8, ACC) + f'<g fill="{ACC}" opacity="0.8"><circle cx="128" cy="84" r="2.4"/><circle cx="196" cy="76" r="2.2"/><circle cx="150" cy="70" r="1.8"/><circle cx="182" cy="94" r="2"/><circle cx="140" cy="104" r="1.6"/></g>' + s_path("M160 104 q10 -8 20 -4 M160 104 q-10 -8 -20 -4", 1.4, INK),
 "boot": s_path("M120 60 L120 150 L118 168 L206 168 L206 152 L148 142 L148 60z", 2.6) + s_path("M148 60 L148 142", 1.4) + f'<g stroke="{ACC}" stroke-width="1.6" fill="none"><path d="M118 168 q-6 8 -2 14 M206 168 q6 8 2 14"/></g>',
 "ghost": s_path("M160 52 q-40 10 -40 62 l0 56 q10 -10 20 0 q10 -10 20 0 q10 -10 20 0 q10 -10 20 0 l0 -56 q0 -52 -40 -62z", 2.4) + f'<circle cx="146" cy="100" r="3.4" fill="{INK}"/><circle cx="174" cy="100" r="3.4" fill="{INK}"/>' + s_path("M150 122 q10 8 20 0", 1.8, ACC),
 "fox": s_path("M110 150 q20 -40 60 -40 q40 0 52 30 q8 22 -8 30 l-88 0 q-22 -2 -16 -20z", 2.4) + s_path("M148 112 l-14 -26 M172 110 l8 -28", 2.4) + f'<path d="M136 88 l-8 -22 l18 12z M180 84 l6 -24 l8 22z" fill="{INK}"/>' + f'<circle cx="188" cy="128" r="2.6" fill="{INK}"/>' + s_path("M216 158 q22 6 30 -6", 2.4, ACC),
 "bottle": s_path("M146 60 L146 92 q-22 12 -22 46 q0 30 36 30 q36 0 36 -30 q0 -34 -22 -46 L174 60z", 2.4) + s_path("M140 60 L180 60", 2.4) + s_path("M160 108 q-8 14 0 26", 1.6, ACC),
 "crops": ''.join(f'<path d="M{x} 180 L{x} 130" stroke="{INK}" stroke-width="1.6"/>' for x in range(96, 226, 18)) + s_path("M96 180 L226 180", 2) + s_path("M170 128 L170 84 M170 100 L156 88 M170 100 L184 86", 2.6, ACC) + f'<circle cx="170" cy="80" r="6" fill="{ACC}"/>',
 "mansion": f'<rect x="86" y="92" width="148" height="88" fill="none" stroke="{INK}" stroke-width="2.4"/>' + s_path("M72 92 L160 52 L248 92", 2.8) + f'<rect x="146" y="130" width="28" height="50" fill="none" stroke="{ACC}" stroke-width="2"/>' + s_path("M96 120 l20 0 M204 120 l20 0 M96 150 l20 0 M204 150 l20 0", 1.6),
 "river": ''.join(f'<path d="M60 {y} q30 -10 60 0 q30 10 60 0 q30 -10 60 0" stroke="{BLUE}" stroke-width="2" fill="none" opacity="0.8"/>' for y in range(140, 190, 16)) + s_path("M120 120 L204 84", 2.2) + s_path("M120 120 L120 152", 1.6) + s_path("M204 84 q10 -4 14 2 M204 84 q4 -10 12 -10", 1.6, ACC) + f'<path d="M96 146 q10 -8 20 0 q-4 8 -10 8 q-8 0 -10 -8z" fill="{INK}"/>',
 "rope": s_path("M160 180 L160 40", 2) + s_path("M154 60 L166 60 M154 80 L166 80 M154 100 L166 100 M154 120 L166 120 M154 140 L166 140 M154 160 L166 160", 1.6) + f'<circle cx="160" cy="32" r="12" fill="{ACC}" opacity="0.9"/>' + s_path("M160 20 q6 -6 0 -12", 1.4, ACC),
 "peartree": s_path("M160 180 L160 96 M160 140 L122 116 M160 140 L198 112 M160 118 L142 100", 2.6) + ''.join(f'<circle cx="{x}" cy="{y}" r="8" fill="{GOLD}" stroke="{INK}" stroke-width="1.2"/>' for x, y in [(122,110),(198,106),(142,94),(176,96),(160,76)]) + s_path("M120 180 L200 180", 2),
 "moonwall": f'<path d="M84 180 L84 96 L236 96 L236 180" stroke="{INK}" stroke-width="2.6" fill="none"/>' + f'<circle cx="160" cy="66" r="20" fill="#f5e6b8" stroke="{INK}" stroke-width="1.6"/>' + s_path("M160 180 L160 128 M146 146 L174 146", 2.2, ACC) + s_path("M96 180 L120 160 M224 180 L200 160", 1.6),
 "pagoda": s_path("M120 180 L200 180 L196 160 L124 160z M128 160 L192 160 L188 138 L132 138z M136 138 L184 138 L180 116 L140 116z M148 116 L172 116 L168 96 L152 96z", 2.2) + s_path("M152 96 L168 96 L164 78 L156 78z", 2.2, ACC) + s_path("M160 78 L160 62", 2, ACC),
 "snake": s_path("M96 150 q40 -50 80 0 q40 50 64 6 q14 -26 -12 -34", 3.2) + f'<circle cx="230" cy="118" r="8" fill="{INK}"/>' + f'<circle cx="233" cy="115" r="1.6" fill="#fff"/>' + f'<path d="M238 122 l10 4 l-10 4z" fill="{ACC}"/>' + s_path("M96 168 L120 168", 2),
 "python": s_path("M80 130 q50 -56 100 0 q44 52 88 -2", 4.4) + f'<circle cx="268" cy="122" r="10" fill="{INK}"/>' + s_path("M60 100 L110 150", 2.6, ACC) + s_path("M60 100 l-8 -14 M60 100 l14 -10", 2, ACC),
 "dog": s_path("M120 150 q10 -30 44 -30 q34 0 44 28 l0 24 l-88 0z", 2.4) + f'<path d="M150 122 l-12 -20 l22 8z M196 122 l12 -20 l-22 8z" fill="{INK}"/>' + f'<circle cx="158" cy="138" r="2.6" fill="{INK}"/><circle cx="182" cy="138" r="2.6" fill="{INK}"/>' + s_path("M120 168 L216 168", 2),
 "wedding": f'<g stroke="{ACC}" stroke-width="1.6" fill="#c94f4f"><line x1="120" y1="60" x2="120" y2="72"/><ellipse cx="120" cy="84" rx="10" ry="12"/><line x1="200" y1="60" x2="200" y2="72"/><ellipse cx="200" cy="84" rx="10" ry="12"/></g>' + s_path("M104 118 Q160 92 216 118", 2.4) + f'<path d="M150 140 l20 -14 l20 14 l-20 14z" fill="none" stroke="{GOLD}" stroke-width="2.2"/>' + ''.join(f'<circle cx="{x}" cy="{y}" r="2" fill="{ACC}"/>' for x,y in [(100,150),(220,148),(140,112),(182,110),(160,160)]),
}
ACCENT_EXTRA = {
 "swirl": s_path("M210 150 q14 -18 0 -34 q-14 -16 0 -32", 1.6, "#7a86a8"),
 "window": f'<rect x="230" y="120" width="44" height="52" fill="none" stroke="{INK}" stroke-width="1.6"/>' + s_path("M252 120 L252 172", 1.2),
 "claw": f'<g stroke="{ACC}" stroke-width="1.6" fill="none"><path d="M100 60 l8 12 M112 56 l6 13 M124 54 l4 14"/></g>',
 "petals": ''.join(f'<circle cx="{x}" cy="{y}" r="2" fill="{ACC}" opacity="0.75"/>' for x, y in [(60,80),(250,120),(70,170),(245,170),(100,50)]),
 "fish": f'<path d="M120 130 q10 -8 20 0 q-4 8 -10 8 q-8 0 -10 -8z" fill="{INK}" opacity="0.8"/>',
 "peach": f'<circle cx="88" cy="150" r="10" fill="{ACC}" opacity="0.85"/>',
 "sickle": s_path("M236 60 q20 20 0 44", 2.4, ACC),
 "crowd": ''.join(f'<circle cx="{x}" cy="176" r="5" fill="{INK}" opacity="0.55"/>' for x in [80,105,215,240]),
 "axe": s_path("M236 120 L258 100 M252 96 q14 2 12 16", 2.4, ACC),
 "monk": s_path("M88 150 q0 -22 16 -22 q16 0 16 22 l-4 26 l-24 0z", 2, INK) + f'<circle cx="104" cy="120" r="9" fill="none" stroke="{INK}" stroke-width="2"/>',
 "basket": f'<path d="M232 148 l28 0 l-6 22 l-16 0z" fill="none" stroke="{ACC}" stroke-width="2"/>',
 "gate": f'<path d="M60 180 L60 120 L96 120 L96 180 M224 180 L224 120 L260 120 L260 180" stroke="{INK}" stroke-width="2" fill="none"/>',
}


MOTIFS.update({
 "storm": f'<path d="M80 90 q20 -28 48 -16 q10 -20 34 -12 q26 -14 40 8 q24 -4 28 20 l0 8 l-150 0z" fill="{INK}" opacity="0.85"/>' + ''.join(f'<line x1="{x}" y1="112" x2="{x-6}" y2="132" stroke="{BLUE}" stroke-width="2"/>' for x in range(100, 250, 22)) + ''.join(f'<circle cx="{x}" cy="{y}" r="2.4" fill="#f5e6b8"/>' for x, y in [(110,142),(150,152),(190,144),(230,150)]),
 "sorcerer": s_path("M110 170 q50 -30 100 0", 2.2) + f'<circle cx="160" cy="108" r="14" fill="none" stroke="{INK}" stroke-width="2.2"/>' + s_path("M160 122 L160 170 M136 140 L184 140 M160 132 L140 118 M160 132 L180 118", 2.2) + f'<path d="M146 94 l14 -18 l14 18z" fill="{ACC}"/>',
 "bones": s_path("M96 168 L224 168", 2) + ''.join(f'<ellipse cx="{x}" cy="160" rx="10" ry="5" fill="none" stroke="{INK}" stroke-width="1.8" transform="rotate({r} {x} 160)"/>' for x, r in [(120,-20),(150,15),(180,-10),(210,20)]) + f'<circle cx="150" cy="120" r="12" fill="none" stroke="{INK}" stroke-width="2"/><circle cx="146" cy="118" r="1.8" fill="{INK}"/><circle cx="154" cy="118" r="1.8" fill="{INK}"/>',
 "wheel": f'<circle cx="160" cy="120" r="46" fill="none" stroke="{INK}" stroke-width="2.6"/>' + f'<circle cx="160" cy="120" r="8" fill="{ACC}"/>' + ''.join(f'<line x1="160" y1="120" x2="{160+44*__import__("math").cos(a)}" y2="{120+44*__import__("math").sin(a)}" stroke="{INK}" stroke-width="1.8"/>' for a in [0, 1.05, 2.09, 3.14, 4.19, 5.24]),
 "ghosts": ''.join(f'<path d="M{x} {y+8} q-14 4 -14 24 l0 22 q5 -5 9 0 q4 -5 9 0 l0 -22 q0 -20 -4 -24z" fill="none" stroke="{INK}" stroke-width="1.8"/>' for x, y in [(110, 96), (160, 82), (208, 100)]) + ''.join(f'<circle cx="{x}" cy="{y}" r="2" fill="{INK}"/>' for x, y in [(104, 122), (154, 108), (202, 126)]),
 "maiden": s_path("M160 60 q-26 8 -26 40 q0 30 18 40 l16 0 q18 -10 18 -40 q0 -32 -26 -40z", 2.2) + f'<circle cx="160" cy="52" r="13" fill="none" stroke="{INK}" stroke-width="2.2"/>' + s_path("M160 65 L160 90", 1.6, ACC),
 "sword": s_path("M96 168 L200 84", 3) + s_path("M188 78 L212 98", 2.4) + s_path("M96 168 l-12 10", 2.4, ACC),
 "scholar": s_path("M120 168 L200 168", 2) + f'<rect x="132" y="120" width="56" height="34" fill="none" stroke="{INK}" stroke-width="2"/>' + s_path("M140 132 l40 0 M140 142 l28 0", 1.4, ACC) + f'<circle cx="110" cy="150" r="10" fill="none" stroke="{INK}" stroke-width="2"/>',
 "coins": ''.join(f'<circle cx="{x}" cy="{y}" r="10" fill="none" stroke="{ACC}" stroke-width="2.2"/>' for x, y in [(130,120),(160,108),(190,122),(145,148),(175,150)]) + ''.join(f'<rect x="{x-3}" y="{y-3}" width="6" height="6" fill="{ACC}"/>' for x, y in [(130,120),(160,108),(190,122),(145,148),(175,150)]),
 "island": s_path("M60 150 Q160 120 260 150 L260 168 L60 168z", 2.2) + f'<rect x="140" y="108" width="40" height="42" fill="none" stroke="{INK}" stroke-width="2"/>' + s_path("M132 108 L160 88 L188 108", 2.2) + f'<circle cx="230" cy="70" r="16" fill="#f5e6b8" stroke="{INK}" stroke-width="1.2"/>',
 "temple": MOTIFS["pagoda"].replace("M120", "M110").replace("M200", "M190"),
 "scroll": s_path("M110 100 L210 100 L210 150 L110 150z", 2.2) + s_path("M110 112 L210 112 M110 126 L210 126 M110 140 L196 140", 1.4, ACC),
 "eagletiger": f'<path d="M110 130 q30 -34 60 -10 q10 -20 34 -8 q22 12 8 34 q-16 24 -50 12 q-34 12 -52 -10z" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<circle cx="196" cy="122" r="3" fill="{ACC}"/>' + s_path("M130 108 q10 -14 26 -12", 2, ACC),
 "cricket": s_path("M130 140 q30 -24 60 0 q-8 20 -30 20 q-22 0 -30 -20z", 2.2) + s_path("M150 122 L146 108 M170 122 L176 108", 1.8) + s_path("M190 140 q16 -4 20 -16", 2, ACC) + ''.join(f'<line x1="{x}" y1="150" x2="{x-4}" y2="162" stroke="{INK}" stroke-width="1.6"/>' for x in [140, 155, 172, 186]),
 "skinmask": f'<path d="M128 92 q32 -22 64 0 q10 30 0 60 q-32 22 -64 0 q-10 -30 0 -60z" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<path d="M128 92 q32 14 64 0 l0 22 q-32 14 -64 0z" fill="{ACC}" opacity="0.25"/>' + f'<circle cx="148" cy="128" r="3" fill="{INK}"/><circle cx="172" cy="128" r="3" fill="{INK}"/>' + s_path("M148 148 q12 8 24 0", 1.8, ACC),
})


MOTIFS.update({
 "flame": f'<path d="M160 160 q-18 -22 0 -46 q6 10 10 14 q-2 -18 12 -30 q10 22 2 34 q10 -6 8 -18 q16 20 4 46 q-16 14 -34 0z" fill="none" stroke="{ACC}" stroke-width="2.2"/>' + f'<circle cx="160" cy="140" r="5" fill="{GOLD}"/>',
 "stone": f'<ellipse cx="160" cy="150" rx="46" ry="26" fill="none" stroke="{INK}" stroke-width="2.4"/>' + s_path("M138 142 q10 8 20 2 M160 154 q10 -6 18 2", 1.6, ACC),
 "judge": f'<rect x="116" y="76" width="88" height="88" rx="6" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<path d="M128 76 q32 -18 64 0 l0 14 q-32 -14 -64 0z" fill="{INK}"/>' + f'<circle cx="144" cy="112" r="3" fill="{INK}"/><circle cx="176" cy="112" r="3" fill="{INK}"/>' + s_path("M146 140 q14 8 28 0", 1.8, ACC) + s_path("M160 90 L160 100", 1.6),
 "flower": ''.join(f'<circle cx="{160+26*__import__("math").cos(a):.0f}" cy="{118+26*__import__("math").sin(a):.0f}" r="11" fill="none" stroke="{ACC}" stroke-width="2"/>' for a in [0.4, 1.45, 2.5, 3.55, 4.6]) + f'<circle cx="160" cy="118" r="8" fill="{GOLD}"/>' + s_path("M160 126 q-4 30 -18 44 M160 140 q8 4 14 -2", 2, INK),
 "mice": f'<ellipse cx="140" cy="140" rx="18" ry="12" fill="none" stroke="{INK}" stroke-width="2.2"/>' + f'<circle cx="158" cy="134" r="8" fill="none" stroke="{INK}" stroke-width="2.2"/>' + s_path("M166 132 q10 -2 12 6", 1.6, ACC) + f'<ellipse cx="196" cy="150" rx="16" ry="10" fill="none" stroke="{INK}" stroke-width="2.2"/>' + s_path("M150 146 q24 10 34 4", 1.4, ACC),
 "quake": ''.join(f'<path d="M{x} {y} l10 -8 l10 8 l10 -8" stroke="{ACC}" stroke-width="2" fill="none"/>' for x, y in [(90,90),(120,70),(150,92),(180,66),(210,90)]) + s_path("M60 170 L260 170", 2.4) + s_path("M96 170 L104 148 M210 170 L218 146", 1.6, INK),
 "sea": ''.join(f'<path d="M60 {y} q25 -12 50 0 q25 12 50 0 q25 -12 50 0 q25 12 30 0" stroke="{BLUE}" stroke-width="2.2" fill="none"/>' for y in range(120, 180, 18)) + s_path("M150 96 q10 -12 24 -8 q-4 12 -16 14z", 2, INK),
 "whale": s_path("M70 130 q60 -40 130 -12 q30 10 40 -6 q10 22 -12 30 q-80 30 -158 -12z", 2.6) + f'<circle cx="216" cy="118" r="2.6" fill="{INK}"/>' + s_path("M96 118 q-6 -14 4 -22", 2, INK),
 "turtle": f'<ellipse cx="160" cy="140" rx="52" ry="34" fill="none" stroke="{INK}" stroke-width="2.4"/>' + s_path("M132 128 q28 -18 56 0 M128 144 q32 14 64 0", 1.6, ACC) + f'<circle cx="222" cy="132" r="11" fill="none" stroke="{INK}" stroke-width="2.2"/>' + f'<circle cx="225" cy="129" r="1.8" fill="{INK}"/>',
 "herb": s_path("M160 176 L160 108 M160 136 q-22 -4 -30 -24 q24 -2 30 18 M160 124 q22 -4 30 -24 q-24 -2 -30 18", 2.2, "#2e7d4f") + f'<circle cx="128" cy="98" r="4" fill="{ACC}"/><circle cx="192" cy="98" r="4" fill="{ACC}"/>',
 "child": f'<circle cx="160" cy="96" r="16" fill="none" stroke="{INK}" stroke-width="2.4"/>' + s_path("M160 112 L160 150 M136 128 L184 128", 2.2) + f'<circle cx="154" cy="94" r="2" fill="{INK}"/><circle cx="166" cy="94" r="2" fill="{INK}"/>' + s_path("M154 102 q6 5 12 0", 1.4, ACC),
 "priest": s_path("M130 172 L190 172 M160 172 L160 96 M136 130 L184 130 M160 104 L142 88 M160 104 L178 88", 2.4) + f'<circle cx="160" cy="82" r="13" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<path d="M146 66 l14 -16 l14 16z" fill="{ACC}"/>',
})


MOTIFS.update({
 "wine": f'<path d="M148 84 q-16 10 -16 30 q0 24 28 24 q28 0 28 -24 q0 -20 -16 -30z" fill="none" stroke="{INK}" stroke-width="2.4"/>' + s_path("M132 96 L172 96 M160 138 L160 168 M148 168 L172 168", 2.2) + f'<circle cx="160" cy="110" r="7" fill="{ACC}" opacity="0.3"/>',
 "dragon": s_path("M80 140 q40 -50 80 -10 q30 34 70 -4", 3) + f'<path d="M228 126 l16 -8 l-6 18z" fill="{INK}"/>' + f'<circle cx="236" cy="118" r="2" fill="{ACC}"/>' + s_path("M104 118 q-8 -14 2 -24", 2, INK),
 "sound": ''.join(f'<path d="M{x} 150 q0 -{h} 0 -{h}" stroke="{ACC}" stroke-width="2.4" fill="none"/>' for x, h in [(120,30),(145,50),(170,70),(195,45),(220,28)]) + s_path("M104 168 L236 168", 2),
})


MOTIFS.update({
 "rain": ''.join(f'<line x1="{x}" y1="80" x2="{x-8}" y2="118" stroke="{BLUE}" stroke-width="1.8"/>' for x in range(100, 240, 16)) + s_path("M90 70 q70 -30 140 0", 2.2, INK),
 "well": s_path("M120 130 L120 180 L200 180 L200 130", 2.4) + s_path("M120 130 Q160 108 200 130", 2.4) + s_path("M160 132 L160 178 M140 142 L140 178 M180 142 L180 178", 1.4, ACC) + f'<circle cx="228" cy="96" r="14" fill="{GOLD}" stroke="{INK}" stroke-width="1.4"/>',
 "yaksha": s_path("M140 150 q-14 -36 10 -58 q22 -20 40 0 q24 22 10 58 l-8 26 l-44 0z", 2.4) + f'<circle cx="158" cy="116" r="3" fill="{ACC}"/><circle cx="182" cy="116" r="3" fill="{ACC}"/>' + s_path("M150 138 q20 12 40 0 M160 92 l-6 -14 M178 92 l6 -14", 2, INK),
})


MOTIFS.update({
 "ball": f'<circle cx="160" cy="128" r="26" fill="none" stroke="{INK}" stroke-width="2.4"/>' + s_path("M142 110 q18 18 36 0 M134 128 l52 0 M142 146 q18 -18 36 0", 1.8, ACC),
 "weave": ''.join(f'<line x1="120" y1="{y}" x2="200" y2="{y}" stroke="{INK}" stroke-width="1.4"/>' for y in range(90, 170, 12)) + ''.join(f'<line x1="{x}" y1="82" x2="{x}" y2="172" stroke="{ACC}" stroke-width="0.9" opacity="0.7"/>' for x in range(126, 200, 10)),
 "bird": f'<ellipse cx="160" cy="128" rx="22" ry="16" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<circle cx="184" cy="112" r="10" fill="none" stroke="{INK}" stroke-width="2.2"/>' + f'<path d="M194 110 l12 3 l-12 4z" fill="{ACC}"/>' + s_path("M138 138 q-14 8 -18 20", 2, INK),
 "wave": ''.join(f'<path d="M60 {y} q25 -12 50 0 q25 12 50 0 q25 -12 50 0" stroke="{BLUE}" stroke-width="2" fill="none"/>' for y in range(110, 180, 18)),
})


MOTIFS.update({
 "leaf": s_path("M160 170 L160 100 M160 100 q-30 -18 -44 4 q30 18 44 -4 M160 100 q30 -18 44 4 q-30 18 -44 4", 2.2, "#2e7d4f"),
 "mask": f'<path d="M128 92 q32 -22 64 0 q10 30 0 60 q-32 22 -64 0 q-10 -30 0 -60z" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<circle cx="148" cy="122" r="5" fill="{ACC}"/><circle cx="172" cy="122" r="5" fill="{ACC}"/>' + s_path("M140 146 q20 -10 40 0", 2, INK),
})


MOTIFS.update({
 "pot": s_path("M120 130 q0 -20 40 -20 q40 0 40 20 l-6 8 l-68 0z", 2.4) + s_path("M116 138 L204 138 M120 150 L200 150", 2.2) + f'<circle cx="150" cy="112" r="4" fill="{ACC}" opacity="0.6"/><circle cx="168" cy="106" r="3" fill="{ACC}" opacity="0.45"/>' + s_path("M140 96 q4 -12 -2 -20 M166 92 q6 -10 0 -18", 1.6, "#7a86a8"),
 "chess": f'<rect x="116" y="84" width="88" height="88" fill="none" stroke="{INK}" stroke-width="2.2"/>' + ''.join(f'<line x1="{x}" y1="84" x2="{x}" y2="172" stroke="{INK}" stroke-width="0.8"/>' for x in range(138, 204, 22)) + ''.join(f'<line x1="116" y1="{y}" x2="204" y2="{y}" stroke="{INK}" stroke-width="0.8"/>' for y in range(106, 172, 22)) + f'<circle cx="160" cy="128" r="7" fill="{ACC}"/><circle cx="138" cy="106" r="5" fill="{INK}"/>',
 "lantern": f'<g stroke="{ACC}" stroke-width="1.8" fill="#c94f4f"><line x1="150" y1="70" x2="150" y2="86"/><ellipse cx="150" cy="108" rx="18" ry="24"/><line x1="186" y1="78" x2="186" y2="92"/><ellipse cx="186" cy="112" rx="14" ry="19"/></g>' + s_path("M132 108 L168 108 M138 94 L162 94 M138 122 L162 122", 1, "#f8f3e6"),
})


MOTIFS.update({
 "frog": f'<ellipse cx="160" cy="140" rx="30" ry="20" fill="none" stroke="{INK}" stroke-width="2.4"/>' + f'<circle cx="148" cy="122" r="6" fill="none" stroke="{INK}" stroke-width="2"/><circle cx="172" cy="122" r="6" fill="none" stroke="{INK}" stroke-width="2"/>' + s_path("M140 168 q20 10 40 0", 2, INK),
 "lotus": ''.join(f'<path d="M160 150 q-{d} -34 -{d*2} -18 q{d} 26 {d*2} 26z" fill="none" stroke="{ACC}" stroke-width="2"/>' for d in [10, 22, 34]) + ''.join(f'<path d="M160 150 q{d} -34 {d*2} -18 q-{d} 26 -{d*2} 26z" fill="none" stroke="{ACC}" stroke-width="2"/>' for d in [10, 22, 34]) + s_path("M160 176 L160 150", 2.4, "#2e7d4f"),
 "mantis": s_path("M150 160 L150 110 M150 122 L124 96 M150 122 L176 96 M150 136 L126 146 M150 136 L174 146", 2.4, "#2e7d4f") + f'<circle cx="150" cy="100" r="9" fill="none" stroke="#2e7d4f" stroke-width="2.2"/>' + f'<circle cx="146" cy="98" r="1.6" fill="#2e7d4f"/><circle cx="154" cy="98" r="1.6" fill="#2e7d4f"/>',
})

data = {}
for f in ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'aa']:
    data.update(json.load(open(f'/tmp/lz_data/{f}.json')))
texts = json.load(open('/tmp/liaozhai_2_40.json'))
texts.update(json.load(open('/tmp/liaozhai_41_60.json')))
texts.update(json.load(open('/tmp/liaozhai_61_80.json')))
texts.update(json.load(open('/tmp/liaozhai_81_100.json')))
texts.update(json.load(open('/tmp/liaozhai_101_120.json')))
texts.update(json.load(open('/tmp/liaozhai_121_140.json')))
texts.update(json.load(open('/tmp/liaozhai_141_160.json')))
texts.update(json.load(open('/tmp/liaozhai_161_180.json')))
sid2key = {sid: f"{i+2:03d}" for i, sid in enumerate(texts.keys())}
titles = json.load(open('/tmp/lzbuild/titles.json'))

JAR = '/tmp/lz_ck.txt'
subprocess.run(['curl', '-s', '-c', JAR, '-H', 'Content-Type: application/json',
                '-d', '{"username":"admin","password":"root@123"}',
                'http://localhost:8080/api/login', '-o', '/dev/null'], check=False)

def upload(local, key):
    r = subprocess.run(['curl', '-s', '-b', JAR, '-X', 'PUT', '--data-binary', f'@{local}',
                        f'http://localhost:8080/api/oss/{quote(key)}'], capture_output=True, text=True)
    return '"ok":true' in r.stdout

ok = 0
for key in sorted(data.keys()):
    d = data[key]
    sid = [s for s, k in sid2key.items() if k == key][0]
    orig = texts[sid]['orig']
    notes_md = "\n".join(f"- **{k2}** — {v}" for k2, v in d['notes'])
    orig_md = "\n".join(f"{i}. {p}\n" for i, p in enumerate(orig, 1))
    gengai = "\n".join(f"- {x}" for x in d['gengai'])
    people = "\n".join(f"| {a} | {b} | {c} |" for a, b, c in d['people'])
    prev_k = f"{int(key)-1:03d}"
    nxt_k = f"{int(key)+1:03d}"
    prev_t = titles.get(prev_k)
    nxt_t = titles.get(nxt_k)
    links = ""
    if prev_t:
        links += f"*上一篇:[{prev_t}]({prev_k}-{prev_t}.md)*　"
    links += f"*下一篇:[{nxt_t}]({nxt_k}-{nxt_t}.md)*" if nxt_t else "*下一篇:待整理*"
    md = f"""# {d['title']}

> 《聊斋志异》卷一 · 第 {int(key)} 篇 · 整理:Claude(原文为清代蒲松龄所著,公有领域;注释与评析为原创整理)

![插图]({d['title']}.svg)

---

## 一、原文

{orig_md}
---

## 二、注释

{notes_md}

---

## 三、白话意译

{d['yiyi']}

---

## 四、内容梗概

{gengai}

## 五、人物

| 人物 | 身份 | 要点 |
|---|---|---|
{people}

## 六、评析

{d['pingxi']}

## 七、本篇名句

> {d['mingju']}

---

{links}
"""
    md_path = f"/tmp/liaozhai/{key}-{d['title']}.md"
    open(md_path, 'w').write(md)
    m = d['motifs']
    main = MOTIFS.get(m['main'], MOTIFS['ghost'])
    acc = ACCENT_EXTRA.get(m.get('accent', ''), "")
    svg = frame(main + acc, f"聊斋志异 · 卷一 · 第 {int(key)} 篇")
    svg_path = f"/tmp/liaozhai/{key}-{d['title']}.svg"
    open(svg_path, 'w').write(svg)
    vol = texts[sid].get('vol', '卷一')
    k1 = f"聊斋/{vol}/{key}-{d['title']}.md"
    k2 = f"聊斋/{vol}/{key}-{d['title']}.svg"
    if upload(md_path, k1) and upload(svg_path, k2):
        ok += 1
    else:
        print('FAILED:', key)
print(f'uploaded {ok}/19')
