/**
 * Static site copy for the Donate and Support pages. Editorial content, not
 * catalog data — served through `source.ts` so screens keep a single data seam.
 */

export const donateTiers = [
	{
		key: 'supporter',
		name: 'Supporter',
		price: '$3',
		accent: 'var(--k-ongoing)',
		popular: false,
		perks: [
			'Ad-free reading, forever',
			'Supporter badge on your profile',
			'Early access to new chapters',
		],
	},
	{
		key: 'patron',
		name: 'Patron',
		price: '$8',
		accent: 'var(--k-hiatus)',
		popular: true,
		perks: [
			'Everything in Supporter',
			'Unlimited offline downloads',
			'Vote on which series get licensed next',
			'Members-only comment lounge',
		],
	},
	{
		key: 'founder',
		name: 'Founder',
		price: '$20',
		accent: 'var(--k-accent-purple)',
		popular: false,
		perks: [
			'Everything in Patron',
			'Name in the credits page',
			'Two guest passes to gift friends',
			'Direct line to the komiq team',
		],
	},
];

export const donateAmounts = [5, 15, 30, 50, 100];

export const donateAllocation = [
	{
		pct: '62%',
		color: 'var(--k-accent)',
		label: 'Creators & translators',
		desc: 'Paid directly to the artists, writers, and localization teams behind each series.',
	},
	{
		pct: '28%',
		color: 'var(--k-hiatus)',
		label: 'Servers & delivery',
		desc: 'Fast, global image hosting so pages load instantly, anywhere.',
	},
	{
		pct: '10%',
		color: 'var(--k-ongoing)',
		label: 'Platform & tools',
		desc: 'A small team keeping komiq running, safe, and ad-free.',
	},
];

export const supportCategories = [
	{
		icon: 'user',
		title: 'Account',
		desc: 'Sign-in, profile, and privacy settings.',
		count: 14,
		iconBg: 'rgba(127,211,154,0.14)',
		iconColor: 'var(--k-ongoing)',
	},
	{
		icon: 'book',
		title: 'Reading',
		desc: 'Reader modes, downloads, and formats.',
		count: 21,
		iconBg: 'rgba(198,156,240,0.14)',
		iconColor: 'var(--k-accent-purple)',
	},
	{
		icon: 'card',
		title: 'Billing & membership',
		desc: 'Donations, tiers, and receipts.',
		count: 9,
		iconBg: 'rgba(224,179,84,0.14)',
		iconColor: 'var(--k-hiatus)',
	},
	{
		icon: 'flag',
		title: 'Report an issue',
		desc: 'Broken pages, wrong info, or abuse.',
		count: 7,
		iconBg: 'rgba(224,131,105,0.14)',
		iconColor: 'var(--k-accent)',
	},
];

export const faqs = [
	{
		q: 'Is komiq free to use?',
		a: 'Yes. Reading is free and always will be. Members who donate get perks like ad-free browsing, offline downloads, and early chapters, but every series is readable without paying.',
	},
	{
		q: "What's the difference between manga, manhwa, and manhua?",
		a: 'They refer to comics from different regions: manga (Japan, read right-to-left), manhwa (Korea, usually full-color vertical scroll), and manhua (China). You can filter by any of these formats on the Browse page.',
	},
	{
		q: 'How do I download chapters to read offline?',
		a: "Offline downloads are a Patron-tier perk. Once you're a member, tap the download icon on any chapter or series page and it'll be available in your Library without a connection.",
	},
	{
		q: 'Can I change the reading direction and page size?',
		a: 'Absolutely. Open any chapter and tap the settings icon to switch between long-strip and single-page modes and adjust page width. Your preferences are remembered per device.',
	},
	{
		q: 'How do I cancel or change my membership?',
		a: 'Go to your Profile settings and open Membership. You can upgrade, downgrade, or cancel at any time — changes take effect at the end of your current billing cycle.',
	},
	{
		q: "A chapter is missing pages or won't load. What do I do?",
		a: 'First try refreshing. If it persists, use the Report an issue option on the chapter page so our team can re-process the file, or reach out via live chat below.',
	},
];
