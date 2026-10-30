import CoreGraphics
import CoreImage
import Darwin
import Foundation
import Vision

private enum HelperError: Error, CustomStringConvertible {
    case usage(String)
    case unsupportedRevision(Int)
    case loadFailed(String)
    case noObservation
    case noInstances
    case outputExists(String)
    case createOutputFailed(String)
    case writeFailed(String)

    var description: String {
        switch self {
        case .usage(let message): return message
        case .unsupportedRevision(let revision): return "unsupported Vision revision: \(revision)"
        case .loadFailed(let path): return "failed to load input image: \(path)"
        case .noObservation: return "Vision returned no foreground mask observation"
        case .noInstances: return "Vision returned no foreground instances"
        case .outputExists(let path): return "Vision output directory already exists: \(path)"
        case .createOutputFailed(let path): return "failed to create Vision output directory: \(path)"
        case .writeFailed(let path): return "failed to write Vision artifact: \(path)"
        }
    }
}

private struct Arguments {
    let input: String
    let outputDirectory: String
    let revision: Int
}

private func parseArguments(_ arguments: [String]) throws -> Arguments {
    guard arguments.first == "foreground-instances" else {
        throw HelperError.usage("usage: perfectpixel-vision-helper foreground-instances --input <absolute> --output-dir <absolute> --revision 1")
    }
    var values: [String: String] = [:]
    var index = 1
    while index < arguments.count {
        let key = arguments[index]
        guard key.hasPrefix("--"), index + 1 < arguments.count else {
            throw HelperError.usage("every option requires one value")
        }
        guard values[key] == nil else {
            throw HelperError.usage("duplicate option: \(key)")
        }
        values[key] = arguments[index + 1]
        index += 2
    }
    guard values.count == 3,
          let input = values["--input"],
          let outputDirectory = values["--output-dir"],
          let revisionText = values["--revision"],
          let revision = Int(revisionText),
          input.hasPrefix("/"), outputDirectory.hasPrefix("/") else {
        throw HelperError.usage("--input, --output-dir and --revision are required; paths must be absolute")
    }
    return Arguments(input: input, outputDirectory: outputDirectory, revision: revision)
}

@available(macOS 14.0, *)
private func foregroundInstances(_ arguments: Arguments) throws {
    guard arguments.revision == 1 else {
        throw HelperError.unsupportedRevision(arguments.revision)
    }
    let inputURL = URL(fileURLWithPath: arguments.input)
    let outputDirectoryURL = URL(fileURLWithPath: arguments.outputDirectory, isDirectory: true)
    guard !FileManager.default.fileExists(atPath: outputDirectoryURL.path) else {
        throw HelperError.outputExists(outputDirectoryURL.path)
    }
    guard let image = CIImage(contentsOf: inputURL) else {
        throw HelperError.loadFailed(arguments.input)
    }

    let request = VNGenerateForegroundInstanceMaskRequest()
    request.revision = VNGenerateForegroundInstanceMaskRequestRevision1
    let handler = VNImageRequestHandler(ciImage: image, options: [:])
    try handler.perform([request])
    guard let observation = request.results?.first else {
        throw HelperError.noObservation
    }
    let instanceIDs = observation.allInstances.sorted()
    guard !instanceIDs.isEmpty else {
        throw HelperError.noInstances
    }

    do {
        try FileManager.default.createDirectory(
            at: outputDirectoryURL,
            withIntermediateDirectories: false
        )
    } catch {
        throw HelperError.createOutputFailed(outputDirectoryURL.path)
    }

    do {
        let context = CIContext(options: [
            .cacheIntermediates: false,
            .useSoftwareRenderer: false,
        ])
        var instances: [[String: Any]] = []
        for instanceID in instanceIDs {
            let buffer = try observation.generateScaledMaskForImage(
                forInstances: IndexSet(integer: instanceID),
                from: handler
            )
            let mask = CIImage(cvPixelBuffer: buffer)
            let fileName = String(format: "mask-%08d.png", instanceID)
            let outputURL = outputDirectoryURL.appendingPathComponent(fileName, isDirectory: false)
            do {
                try context.writePNGRepresentation(
                    of: mask,
                    to: outputURL,
                    format: .L8,
                    colorSpace: CGColorSpaceCreateDeviceGray()
                )
            } catch {
                throw HelperError.writeFailed(outputURL.path)
            }
            instances.append([
                "id": instanceID,
                "file": fileName,
            ])
        }

        let version = ProcessInfo.processInfo.operatingSystemVersion
        let receipt: [String: Any] = [
            "schema": "perfectpixel.apple-vision-helper-receipt/1",
            "provider": "apple_vision",
            "adapterVersion": "1",
            "requestType": "VNGenerateForegroundInstanceMaskRequest",
            "requestRevision": arguments.revision,
            "osVersion": "\(version.majorVersion).\(version.minorVersion).\(version.patchVersion)",
            "instances": instances,
        ]
        let data = try JSONSerialization.data(withJSONObject: receipt, options: [.sortedKeys])
        let receiptURL = outputDirectoryURL.appendingPathComponent("receipt.json", isDirectory: false)
        do {
            try data.write(to: receiptURL, options: [.atomic])
        } catch {
            throw HelperError.writeFailed(receiptURL.path)
        }
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([0x0A]))
    } catch {
        try? FileManager.default.removeItem(at: outputDirectoryURL)
        throw error
    }
}

@main
private enum PerfectPixelVisionHelper {
    static func main() {
        do {
            let arguments = try parseArguments(Array(CommandLine.arguments.dropFirst()))
            guard #available(macOS 14.0, *) else {
                throw HelperError.usage("Apple Vision foreground instance masks require macOS 14.0+")
            }
            try foregroundInstances(arguments)
        } catch {
            let message = "perfectpixel-vision-helper: \(error)\n"
            FileHandle.standardError.write(Data(message.utf8))
            Darwin.exit(2)
        }
    }
}
