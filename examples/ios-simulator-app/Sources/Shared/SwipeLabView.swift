import SwiftUI

struct SwipeLabView: View {
    var body: some View {
        List {
            Section("Pager Variants") {
                NavigationLink("Pager With Indicator") {
                    SwipeLabPagerView(
                        title: "Pager With Indicator",
                        introText: "Swipe left on the content or tap the page indicator.",
                        showsIndicator: true
                    )
                }

                NavigationLink("Pager Without Indicator") {
                    SwipeLabPagerView(
                        title: "Pager Without Indicator",
                        introText: "This variant removes the page indicator, so only the gesture path remains.",
                        showsIndicator: false
                    )
                }

                NavigationLink("Embedded Pager Card") {
                    SwipeLabEmbeddedPagerView()
                }
            }

            Section("Carousel Variants") {
                NavigationLink("Horizontal Card Carousel") {
                    SwipeLabCarouselView()
                }

                NavigationLink("Feed Carousel") {
                    SwipeLabFeedCarouselView()
                }
            }
        }
        .navigationTitle("Swipe Lab")
    }
}

private struct SwipeLabPagerView: View {
    @State private var currentPage = 0

    let title: String
    let introText: String
    let showsIndicator: Bool

    var body: some View {
        TabView(selection: $currentPage) {
            VStack(spacing: 20) {
                Spacer()
                Image(systemName: "rectangle.on.rectangle.angled")
                    .font(.system(size: 48))
                    .foregroundStyle(.teal)
                Text(title)
                    .font(.largeTitle.bold())
                    .multilineTextAlignment(.center)
                Text(introText)
                    .font(.headline)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                Spacer()
            }
            .padding(28)
            .tag(0)

            VStack(alignment: .leading, spacing: 18) {
                Text("\(title) Form")
                    .font(.title.bold())
                Text("If raw swipe works, this screen becomes visible without any fallback tap path.")
                    .foregroundStyle(.secondary)

                TextField("Swipe Lab Email", text: .constant(""))
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .background(.thinMaterial, in: .rect(cornerRadius: 16))

                SecureField("Swipe Lab Password", text: .constant(""))
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .background(.thinMaterial, in: .rect(cornerRadius: 16))

                Spacer()
            }
            .padding(28)
            .tag(1)
        }
        .tabViewStyle(.page(indexDisplayMode: showsIndicator ? .always : .never))
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
        .background(
            LinearGradient(
                colors: [.cyan.opacity(0.10), .orange.opacity(0.08), SwipeLabPalette.background],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        )
    }
}

private struct SwipeLabCarouselView: View {
    private let cards = [
        "Orion Card",
        "Nebula Card",
        "Aurora Card",
        "Comet Card",
    ]

    var body: some View {
        ScrollView(.horizontal) {
            LazyHStack(spacing: 18) {
                ForEach(cards, id: \.self) { card in
                    SwipeLabCard(title: card)
                        .frame(width: 300)
                }
            }
            .padding(.horizontal, 24)
            .padding(.vertical, 40)
        }
        .scrollIndicators(.hidden)
        .navigationTitle("Horizontal Card Carousel")
        .navigationBarTitleDisplayMode(.inline)
        .background(
            LinearGradient(
                colors: [.pink.opacity(0.10), .yellow.opacity(0.08), SwipeLabPalette.background],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        )
    }
}

private struct SwipeLabFeedCarouselView: View {
    private let cards = [
        "Feed Card One",
        "Feed Card Two",
        "Feed Card Three",
        "Feed Card Four",
    ]

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Feed Carousel")
                    .font(.title.bold())
                Text("This simulates a feed with a horizontal carousel embedded inside a vertical scroll.")
                    .foregroundStyle(.secondary)

                RoundedRectangle(cornerRadius: 28)
                    .fill(.indigo.opacity(0.12))
                    .frame(height: 120)
                    .overlay(alignment: .leading) {
                        Text("Hero Banner")
                            .font(.title2.bold())
                            .padding(24)
                    }

                ScrollView(.horizontal) {
                    LazyHStack(spacing: 18) {
                        ForEach(cards, id: \.self) { card in
                            SwipeLabCard(title: card)
                                .frame(width: 300)
                        }
                    }
                    .padding(.horizontal, 4)
                    .padding(.vertical, 8)
                }
                .scrollIndicators(.hidden)

                ForEach(0..<4, id: \.self) { index in
                    RoundedRectangle(cornerRadius: 24)
                        .fill(.secondary.opacity(0.08))
                        .frame(height: 120)
                        .overlay(alignment: .leading) {
                            Text("Feed Section \(index + 1)")
                                .font(.headline)
                                .padding(.horizontal, 20)
                        }
                }
            }
            .padding(20)
        }
        .navigationTitle("Feed Carousel")
        .navigationBarTitleDisplayMode(.inline)
        .background(
            LinearGradient(
                colors: [.mint.opacity(0.10), .indigo.opacity(0.08), SwipeLabPalette.background],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        )
    }
}

private struct SwipeLabEmbeddedPagerView: View {
    @State private var currentPage = 0

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 24) {
                Text("Embedded Pager Card")
                    .font(.largeTitle.bold())
                Text("This simulates a pager nested inside a larger detail screen.")
                    .foregroundStyle(.secondary)

                TabView(selection: $currentPage) {
                    embeddedPagerIntro
                        .tag(0)
                    embeddedPagerForm
                        .tag(1)
                }
                .frame(height: 320)
                .tabViewStyle(.page(indexDisplayMode: .always))

                RoundedRectangle(cornerRadius: 26)
                    .fill(.secondary.opacity(0.08))
                    .frame(height: 160)
                    .overlay(alignment: .leading) {
                        Text("Supporting Content")
                            .font(.headline)
                            .padding(.horizontal, 20)
                    }
            }
            .padding(24)
        }
        .navigationTitle("Embedded Pager Card")
        .navigationBarTitleDisplayMode(.inline)
        .background(
            LinearGradient(
                colors: [.orange.opacity(0.10), .cyan.opacity(0.08), SwipeLabPalette.background],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        )
    }

    private var embeddedPagerIntro: some View {
        VStack(alignment: .leading, spacing: 18) {
            Spacer()
            Text("Embedded Pager Intro")
                .font(.title.bold())
            Text("Swipe this card left without hitting the surrounding scroll view.")
                .foregroundStyle(.secondary)
            Spacer()
        }
        .padding(28)
        .background(.regularMaterial, in: .rect(cornerRadius: 28))
        .padding(.bottom, 24)
    }

    private var embeddedPagerForm: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Embedded Pager Form")
                .font(.title.bold())
            Text("If raw swipe works, this card reveals form fields on the second page.")
                .foregroundStyle(.secondary)

            TextField("Embedded Pager Email", text: .constant(""))
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
                .background(.thinMaterial, in: .rect(cornerRadius: 16))

            SecureField("Embedded Pager Password", text: .constant(""))
                .padding(.horizontal, 14)
                .padding(.vertical, 12)
                .background(.thinMaterial, in: .rect(cornerRadius: 16))

            Spacer()
        }
        .padding(28)
        .background(.regularMaterial, in: .rect(cornerRadius: 28))
        .padding(.bottom, 24)
    }
}

private struct SwipeLabCard: View {
    let title: String

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            Spacer()
            Text(title)
                .font(.title2.bold())
            Text("This card is intentionally wide so swipe flows can reveal it one page at a time.")
                .font(.subheadline)
                .foregroundStyle(.secondary)
        }
        .padding(24)
        .frame(maxHeight: .infinity, alignment: .topLeading)
        .background(.regularMaterial, in: .rect(cornerRadius: 28))
        .overlay {
            RoundedRectangle(cornerRadius: 28)
                .stroke(.white.opacity(0.22), lineWidth: 1)
        }
        .accessibilityElement(children: .combine)
    }
}

private enum SwipeLabPalette {
    static var background: Color {
        #if os(macOS)
        Color(nsColor: .windowBackgroundColor)
        #else
        Color(uiColor: .systemBackground)
        #endif
    }
}
